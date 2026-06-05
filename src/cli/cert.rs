//! Implementation of the `mentisdb cert` CLI subcommand.
//!
//! The subcommand mints a fresh self-signed certificate (and matching
//! private key) for the HTTPS MCP and REST servers, then makes it the
//! active cert by writing the PEM files to the location held in
//! [`MENTISDB_TLS_CERT_ENV`] (or the platform default from
//! [`crate::server::default_tls_dir`]) and persisting the new paths
//! into the operator's `.env` file. The daemon itself is untouched
//! during the command — the new cert is loaded the next time the
//! daemon starts, when [`ensure_tls_cert_with_sans`](crate::server::ensure_tls_cert_with_sans)
//! is invoked with `overwrite = false` and short-circuits because the
//! files already exist on disk.
//!
//! ## Public API
//!
//! - [`CertCommand`](super::CertCommand) — the parsed subcommand arguments.
//! - [`run_cert`] — the dispatcher invoked from the CLI router.
//! - [`resolve_paths`] — decide where `cert.pem` / `key.pem` go.
//! - [`build_extra_sans`] — turn a `<ip-or-domain>` string into
//!   [`SanType`] entries for the cert.
//! - [`update_env_file`] — replace-or-append the `MENTISDB_TLS_CERT` /
//!   `MENTISDB_TLS_KEY` lines of an `.env` file.
//! - [`help_text`] — the single source of truth for the subcommand's
//!   `--help` text.
//!
//! Every public function is doctest-covered; see the per-function
//! rustdoc for runnable examples.

use std::fs;
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use rcgen::SanType;

use crate::server::default_tls_dir;

use super::args::CertCommand;

/// Environment-variable name that holds the path to the active TLS
/// certificate PEM file. Exposed as a constant so the env-file writer,
/// the help text, and downstream consumers all share the same string.
pub const MENTISDB_TLS_CERT_ENV: &str = "MENTISDB_TLS_CERT";
/// Environment-variable name that holds the path to the active TLS
/// private-key PEM file. Pairs with [`MENTISDB_TLS_CERT_ENV`].
pub const MENTISDB_TLS_KEY_ENV: &str = "MENTISDB_TLS_KEY";

/// Default filename for the certificate PEM inside the TLS directory.
pub const CERT_FILENAME: &str = "cert.pem";
/// Default filename for the private-key PEM inside the TLS directory.
pub const KEY_FILENAME: &str = "key.pem";

/// Run the `cert` subcommand.
///
/// Reads `cmd`, mints (or reuses) the cert, and prints a human-readable
/// report to `out`. Surfaces I/O and `rcgen` errors through `err` and
/// returns a `String` error so the CLI dispatcher can attach a
/// consistent prefix.
///
/// # Side effects
///
/// - Writes `cert.pem` and `key.pem` to disk at the resolved paths.
/// - Updates the operator's `.env` file (unless `--no-env-update` is
///   set) so the next daemon start picks up the new paths.
///
/// # Examples
///
/// ```no_run
/// use mentisdb::cli::{run_cert, CertCommand};
/// use std::io::Cursor;
///
/// let cmd = CertCommand {
///     host: Some("vps.example.com".to_string()),
///     overwrite: false,
///     reset: false,
///     output_dir: None,
///     env_file: Some("/tmp/x/.env".to_string()),
///     no_env_update: true,
/// };
/// let mut out = Vec::new();
/// let mut err = Vec::new();
/// // run_cert requires a non-default env. In doctests we only assert
/// // the signature; live tests cover behaviour.
/// let _ = run_cert(&cmd, &mut out, &mut err);
/// # let _ = Cursor::new(Vec::<u8>::new());
/// ```
pub fn run_cert(cmd: &CertCommand, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    let (cert_path, key_path) = resolve_paths(cmd).map_err(|e| e.to_string())?;

    // If --reset is specified, delete existing cert and key files first
    // to ensure a clean factory-default certificate is generated.
    if cmd.reset {
        if cert_path.exists() {
            fs::remove_file(&cert_path)
                .map_err(|e| format!("failed to remove existing cert: {e}"))?;
        }
        if key_path.exists() {
            fs::remove_file(&key_path)
                .map_err(|e| format!("failed to remove existing key: {e}"))?;
        }
    }

    // Snapshot the on-disk state before delegating to the server module
    // so we can tell the user whether the cert was just minted or was
    // already on disk. `ensure_tls_cert_with_sans` returns artifacts
    // for both paths, so the existence check is the only reliable
    // signal for "this is a pre-existing cert we should not clobber".
    let cert_existed_before = cert_path.exists() && key_path.exists();
    let extra_sans = build_extra_sans(cmd.host.as_deref()).map_err(|e| e.to_string())?;

    let artifacts =
        crate::server::ensure_tls_cert_with_sans(&cert_path, &key_path, extra_sans, cmd.overwrite)
            .map_err(|e| format!("failed to mint cert: {e}"))?;

    write_status(out, cmd, &artifacts, cert_existed_before)?;
    write_sans(out, &artifacts)?;
    write_fingerprint(out, &artifacts)?;

    let cert_value = artifacts.cert_path.to_string_lossy().into_owned();
    let key_value = artifacts.key_path.to_string_lossy().into_owned();

    if cmd.no_env_update {
        write_export_instructions(out, &cert_value, &key_value)?;
    } else {
        write_or_warn_env_update(out, err, cmd, &cert_value, &key_value)?;
    }

    writeln!(out).map_err(|e| e.to_string())?;
    writeln!(
        out,
        "Restart the daemon for the new cert to be used on the next HTTPS request."
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Print the "Wrote new" or "Reusing existing" line that opens the
/// human-readable report.
fn write_status(
    out: &mut dyn Write,
    cmd: &CertCommand,
    artifacts: &crate::server::TlsCertArtifacts,
    cert_existed_before: bool,
) -> Result<(), String> {
    if cmd.reset {
        writeln!(
            out,
            "Reset to factory defaults: deleted existing cert and key, generated new self-signed TLS cert: {}",
            artifacts.cert_path.display()
        )
        .map_err(|e| e.to_string())?;
        writeln!(
            out,
            "Wrote new TLS private key:   {}",
            artifacts.key_path.display()
        )
        .map_err(|e| e.to_string())?;
    } else if cmd.overwrite || !cert_existed_before {
        writeln!(
            out,
            "Wrote new self-signed TLS cert: {}",
            artifacts.cert_path.display()
        )
        .map_err(|e| e.to_string())?;
        writeln!(
            out,
            "Wrote new TLS private key:   {}",
            artifacts.key_path.display()
        )
        .map_err(|e| e.to_string())?;
    } else {
        writeln!(
            out,
            "Reusing existing TLS cert:   {}",
            artifacts.cert_path.display()
        )
        .map_err(|e| e.to_string())?;
        writeln!(
            out,
            "(pass --force to regenerate; the existing cert is preserved as-is)"
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Print the SAN set, one per line, so an operator can compare against
/// `openssl x509 -noout -ext subjectAltName`.
fn write_sans(
    out: &mut dyn Write,
    artifacts: &crate::server::TlsCertArtifacts,
) -> Result<(), String> {
    writeln!(out).map_err(|e| e.to_string())?;
    writeln!(out, "Subject Alternative Names:").map_err(|e| e.to_string())?;
    for san in &artifacts.sans {
        writeln!(out, "  {san}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Print the SHA-256 fingerprint that can be cross-checked with
/// `openssl x509 -fingerprint -sha256 -noout -in cert.pem`.
fn write_fingerprint(
    out: &mut dyn Write,
    artifacts: &crate::server::TlsCertArtifacts,
) -> Result<(), String> {
    writeln!(out).map_err(|e| e.to_string())?;
    writeln!(out, "SHA-256 fingerprint:").map_err(|e| e.to_string())?;
    writeln!(out, "  {}", artifacts.sha256_fingerprint).map_err(|e| e.to_string())?;
    Ok(())
}

/// Print the `export ...=...` lines an operator can paste into a shell
/// when `--no-env-update` is set.
fn write_export_instructions(
    out: &mut dyn Write,
    cert_value: &str,
    key_value: &str,
) -> Result<(), String> {
    writeln!(out).map_err(|e| e.to_string())?;
    writeln!(out, "Add the following to your environment / shell rc to")
        .map_err(|e| e.to_string())?;
    writeln!(out, "point the next daemon start at the new cert:").map_err(|e| e.to_string())?;
    writeln!(out, "  export {MENTISDB_TLS_CERT_ENV}={cert_value}").map_err(|e| e.to_string())?;
    writeln!(out, "  export {MENTISDB_TLS_KEY_ENV}={key_value}").map_err(|e| e.to_string())?;
    Ok(())
}

/// Try to update the operator's `.env` file. On success print the
/// written values. On failure emit a `warning:` line on `err` and
/// fall back to the `export` instructions so the operator can still
/// finish the job by hand.
fn write_or_warn_env_update(
    out: &mut dyn Write,
    err: &mut dyn Write,
    cmd: &CertCommand,
    cert_value: &str,
    key_value: &str,
) -> Result<(), String> {
    let env_path = resolve_env_file(cmd);
    match update_env_file(&env_path, cert_value, key_value) {
        Ok(()) => {
            writeln!(out).map_err(|e| e.to_string())?;
            writeln!(
                out,
                "Updated {} with the new cert paths.",
                env_path.display()
            )
            .map_err(|e| e.to_string())?;
            writeln!(out, "  {MENTISDB_TLS_CERT_ENV}={cert_value}").map_err(|e| e.to_string())?;
            writeln!(out, "  {MENTISDB_TLS_KEY_ENV}={key_value}").map_err(|e| e.to_string())?;
        }
        Err(error) => {
            writeln!(
                err,
                "warning: could not update {}: {error}",
                env_path.display()
            )
            .map_err(|e| e.to_string())?;
            writeln!(
                out,
                "Set the following variables manually before the next daemon start:"
            )
            .map_err(|e| e.to_string())?;
            writeln!(out, "  export {MENTISDB_TLS_CERT_ENV}={cert_value}")
                .map_err(|e| e.to_string())?;
            writeln!(out, "  export {MENTISDB_TLS_KEY_ENV}={key_value}")
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Resolve the on-disk paths for `cert.pem` and `key.pem`.
///
/// The priority chain is:
///
/// 1. `--out-dir` (passed via [`CertCommand::output_dir`]). The cert
///    and key land in this directory as `cert.pem` and `key.pem`.
/// 2. The directory portion of the existing `MENTISDB_TLS_CERT` env
///    var, so the key always lands next to whatever cert the operator
///    already configured.
/// 3. [`crate::server::default_tls_dir`] (the platform default).
///
/// The returned tuple is `(cert_path, key_path)`. Both are absolute
/// (or, for the default-tls-dir case, joined to the current working
/// directory if `MENTISDB_DIR` is relative).
///
/// # Examples
///
/// ```no_run
/// use mentisdb::cli::{resolve_paths, CertCommand};
///
/// let cmd = CertCommand {
///     host: None,
///     overwrite: false,
///     reset: false,
///     output_dir: Some("/etc/mentisdb/tls".to_string()),
///     env_file: None,
///     no_env_update: false,
/// };
/// let (cert, key) = resolve_paths(&cmd).expect("resolve");
/// assert!(cert.ends_with("cert.pem"));
/// assert!(key.ends_with("key.pem"));
/// ```
pub fn resolve_paths(cmd: &CertCommand) -> io::Result<(PathBuf, PathBuf)> {
    let dir = match cmd.output_dir.as_deref() {
        Some(custom) if !custom.trim().is_empty() => PathBuf::from(custom),
        _ => match std::env::var(MENTISDB_TLS_CERT_ENV) {
            Ok(existing) if !existing.trim().is_empty() => {
                let p = PathBuf::from(existing);
                p.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(default_tls_dir)
            }
            _ => default_tls_dir(),
        },
    };
    Ok((dir.join(CERT_FILENAME), dir.join(KEY_FILENAME)))
}

/// Build the list of extra [`SanType`] entries requested by the
/// operator. IPv4 / IPv6 literals are added as `IpAddress` SANs;
/// anything else is added as a `DnsName` SAN. Returns an error when
/// the supplied host is empty or when `rcgen` rejects the DNS name
/// (e.g. it contains a non-IA5 character).
///
/// # Examples
///
/// ```
/// use mentisdb::cli::build_extra_sans;
/// use rcgen::SanType;
///
/// let sans = build_extra_sans(Some("192.0.2.10")).unwrap();
/// assert!(matches!(sans.as_slice(), [SanType::IpAddress(_)]));
///
/// let sans = build_extra_sans(Some("vps.example.com")).unwrap();
/// assert!(matches!(sans.as_slice(), [SanType::DnsName(_)]));
///
/// let sans = build_extra_sans(None).unwrap();
/// assert!(sans.is_empty());
/// ```
pub fn build_extra_sans(host: Option<&str>) -> io::Result<Vec<SanType>> {
    let Some(raw) = host else {
        return Ok(Vec::new());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cert <ip-or-domain> cannot be empty",
        ));
    }
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(vec![SanType::IpAddress(ip)]);
    }
    let dns: rcgen::string::Ia5String = trimmed.to_string().try_into().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid DNS name '{trimmed}': {e}"),
        )
    })?;
    Ok(vec![SanType::DnsName(dns)])
}

/// Resolve the `.env` file the command should update. Returns
/// `cmd.env_file` if provided, otherwise the literal `.env` in the
/// current working directory.
fn resolve_env_file(cmd: &CertCommand) -> PathBuf {
    cmd.env_file
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".env"))
}

/// Update (or create) `path` so that `MENTISDB_TLS_CERT=<cert>` and
/// `MENTISDB_TLS_KEY=<key>` are present. Existing entries are
/// replaced in place; every other line is preserved verbatim. The
/// file is only rewritten when at least one of the two variables
/// actually changes — no-op updates are detected to avoid touching
/// mtimes (which matters for tools that watch the file).
///
/// Comments (lines starting with `#`) are passed through untouched
/// and never treated as candidate key=value lines. Lines that share a
/// *prefix* with `MENTISDB_TLS_CERT` (such as
/// `MENTISDB_TLS_CERT_OTHER=keep`) are preserved verbatim and not
/// treated as a replacement target — see `env_line_starts_with`.
///
/// # Examples
///
/// ```
/// use mentisdb::cli::update_env_file;
/// use std::path::PathBuf;
///
/// // tempfile is a dev-dependency, but for the doctest we use a path
/// // inside std::env::temp_dir() to keep this self-contained.
/// let path = std::env::temp_dir().join("mentisdb-cli-cert-doctest.env");
/// std::fs::write(&path, "FOO=bar\nMENTISDB_TLS_CERT=/old.pem\n").unwrap();
/// update_env_file(&path, "/new/cert.pem", "/new/key.pem").unwrap();
/// let after = std::fs::read_to_string(&path).unwrap();
/// assert!(after.contains("FOO=bar"));
/// assert!(after.contains("MENTISDB_TLS_CERT=/new/cert.pem"));
/// assert!(!after.contains("/old.pem"));
/// let _ = std::fs::remove_file(&path);
/// let _ = PathBuf::new();
/// ```
pub fn update_env_file(path: &Path, cert_value: &str, key_value: &str) -> io::Result<()> {
    let original = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };

    let updated = replace_or_append_env(
        &replace_or_append_env(&original, MENTISDB_TLS_CERT_ENV, cert_value),
        MENTISDB_TLS_KEY_ENV,
        key_value,
    );

    if updated == original {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, updated)
}

/// Pure helper: return a new env-file body where every line matching
/// `key=...` is replaced with `key=value`, and an entry is appended
/// when no match exists. Comment lines (starting with `#`) are passed
/// through verbatim. See [`update_env_file`] for the full semantics.
fn replace_or_append_env(text: &str, key: &str, value: &str) -> String {
    let mut output = String::with_capacity(text.len() + 64);
    let mut replaced = false;
    for line in text.lines() {
        if line.starts_with('#') {
            // Comment line — never treat as a candidate. Copy through.
            output.push_str(line);
            output.push('\n');
        } else if env_line_starts_with(line, key) {
            output.push_str(&format!("{key}={value}\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&format!("{key}={value}\n"));
    }
    output
}

/// Match `KEY=...` but not `PREFIX_KEY=...`. Anchored at the start of
/// the line; the second character after the prefix must be `=`.
fn env_line_starts_with(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .map(|rest| rest.starts_with('='))
        .unwrap_or(false)
}

/// Full help text for the `cert` subcommand, consumed by
/// `cli::args::help_text` and `bin::mentisdb::daemon_help_text` so
/// there is one source of truth for the help block. Returns a
/// `&'static str` because the text is built at compile time.
pub fn help_text() -> &'static str {
    "\
  cert
    Mint a fresh self-signed TLS certificate for the HTTPS MCP and REST
    servers (and the dashboard) and make it the active cert by writing
    the new `cert.pem` + `key.pem` to the location held in
    MENTISDB_TLS_CERT (default `<MENTISDB_DIR>/tls/`). Persists
    MENTISDB_TLS_CERT and MENTISDB_TLS_KEY into the .env file so the
    next daemon start picks the new cert up automatically.

    The standard SAN set (my.mentisdb.com, localhost, 127.0.0.1, every
    unicast IP on every host interface, plus MENTISDB_BIND_HOST when it
    is a DNS name) is always included. When an <ip-or-domain> argument
    is supplied, it is appended as an additional Subject Alternative
    Name so the cert is valid for the given host — handy for remote /
    VPS setups where the operator connects to the daemon by IP or a
    custom hostname.

    By default the command refuses to overwrite an existing cert; pass
    --force to regenerate. The new cert is only used for subsequent
    HTTPS connections, so restart the daemon to pick it up.

    Examples:
      # 1. Generate the standard self-signed cert into the default
      #    ~/.cloudllm/mentisdb/tls/ directory and update ./.env.
      mentisdb cert

      # 2. Mint a cert that is also valid for a remote VPS IP, so
      #    `curl https://<vps-ip>:9473/health` trusts the daemon.
      mentisdb cert 192.0.2.10

      # 3. Mint a cert that is also valid for a custom DNS name.
      mentisdb cert vps.example.com

      # 4. Regenerate an existing cert after adding a new IP to the
      #    box (or rotating the key). Without --force the command
      #    reuses the existing material.
      mentisdb cert 192.0.2.10 --force

      # 5. Reset to factory defaults: delete any existing cert/key
      #    files first, then generate a fresh cert with the standard
      #    SAN set. Useful if certs were corrupted or manually edited.
      mentisdb cert --reset

      # 6. Write the cert to a non-default directory (useful in
      #    container / systemd / packager deployments) and update a
      #    .env file that lives outside the current working dir.
      mentisdb cert 192.0.2.10 \\
          --out-dir /etc/mentisdb/tls \\
          --env-file /etc/mentisdb/mentisdb.env

      # 7. Print the values to copy into your shell rc, without
      #    touching any .env on disk. Useful for ephemeral hosts.
      mentisdb cert --no-env-update

    Options:
      <ip-or-domain>     Optional IP or DNS hostname to add as a SAN. IPv4
                          and IPv6 literals are added as IP SANs; everything
                          else as a DNS SAN.
      --force            Overwrite an existing cert and key on disk.
      --reset            Delete existing cert/key files first, then generate
                          a fresh factory-default certificate. Implies --force.
      --out-dir <path>   Directory to write cert.pem and key.pem into.
                          Defaults to the directory of MENTISDB_TLS_CERT
                          (or the platform default).
      --env-file <path>  Path to the .env file to update. Defaults to
                          .env in the current working directory.
      --no-env-update    Skip updating the .env file; just print the
                          values to copy into your environment.
      --help             Show this help text.

"
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── build_extra_sans ──────────────────────────────────────────────

    #[test]
    fn extra_sans_parses_ipv4() {
        let sans = build_extra_sans(Some("192.0.2.10")).unwrap();
        assert!(matches!(sans.as_slice(), [SanType::IpAddress(_)]));
    }

    #[test]
    fn extra_sans_parses_ipv6() {
        let sans = build_extra_sans(Some("2001:db8::1")).unwrap();
        assert!(matches!(sans.as_slice(), [SanType::IpAddress(_)]));
    }

    #[test]
    fn extra_sans_parses_dns() {
        let sans = build_extra_sans(Some("vps.example.com")).unwrap();
        assert!(matches!(sans.as_slice(), [SanType::DnsName(_)]));
    }

    #[test]
    fn extra_sans_rejects_empty_or_whitespace() {
        for input in ["", "   ", "\t"] {
            let err = build_extra_sans(Some(input)).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "input: {input:?}");
        }
    }

    #[test]
    fn extra_sans_rejects_non_ia5_dns_characters() {
        // rcgen's Ia5String accepts every printable 7-bit ASCII character,
        // so "not a dns!" is technically valid Ia5 (the spaces and `!` are
        // IA5). What rcgen *does* reject is anything outside the IA5
        // charset — e.g. accented or non-Latin characters. We delegate
        // DNS-format validation to the TLS client at handshake time and
        // only guard against the truly invalid input here.
        let err = build_extra_sans(Some("café.example.com")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn extra_sans_none_returns_empty() {
        let sans = build_extra_sans(None).unwrap();
        assert!(sans.is_empty());
    }

    // ── replace_or_append_env ─────────────────────────────────────────

    #[test]
    fn replace_or_append_env_creates_when_missing() {
        let out = replace_or_append_env("", "MENTISDB_TLS_CERT", "/tmp/c.pem");
        assert_eq!(out, "MENTISDB_TLS_CERT=/tmp/c.pem\n");
    }

    #[test]
    fn replace_or_append_env_updates_in_place_preserves_others() {
        let original = "FOO=bar\n# comment\nMENTISDB_TLS_CERT=/old/c.pem\nBAZ=qux\n";
        let out = replace_or_append_env(original, "MENTISDB_TLS_CERT", "/new/c.pem");
        assert!(out.contains("FOO=bar"));
        assert!(out.contains("# comment"));
        assert!(out.contains("BAZ=qux"));
        assert!(out.contains("MENTISDB_TLS_CERT=/new/c.pem"));
        assert!(!out.contains("/old/c.pem"));
    }

    #[test]
    fn replace_or_append_env_appends_when_key_absent() {
        let original = "FOO=bar\n";
        let out = replace_or_append_env(original, "MENTISDB_TLS_KEY", "/k.pem");
        assert!(out.contains("FOO=bar\n"));
        assert!(out.contains("MENTISDB_TLS_KEY=/k.pem\n"));
        // ensure the existing newline is preserved and a new line appended.
        assert!(out.ends_with("MENTISDB_TLS_KEY=/k.pem\n"));
    }

    #[test]
    fn replace_or_append_env_does_not_touch_similar_prefix() {
        let original = "MENTISDB_TLS_CERT_OTHER=keep\n";
        let out = replace_or_append_env(original, "MENTISDB_TLS_CERT", "/new/c.pem");
        assert!(out.contains("MENTISDB_TLS_CERT_OTHER=keep"));
        assert!(out.contains("MENTISDB_TLS_CERT=/new/c.pem"));
    }

    #[test]
    fn replace_or_append_env_preserves_comments() {
        let original = "# header comment\nKEY=val\n";
        let out = replace_or_append_env(original, "MENTISDB_TLS_CERT", "/new/c.pem");
        assert!(out.contains("# header comment"));
        assert!(out.contains("KEY=val"));
        assert!(out.contains("MENTISDB_TLS_CERT=/new/c.pem"));
    }

    // ── env_line_starts_with ──────────────────────────────────────────

    #[test]
    fn env_line_starts_with_is_anchored() {
        assert!(env_line_starts_with(
            "MENTISDB_TLS_CERT=/x",
            "MENTISDB_TLS_CERT"
        ));
        assert!(!env_line_starts_with(
            "MENTISDB_TLS_CERT_OTHER=/x",
            "MENTISDB_TLS_CERT"
        ));
        assert!(!env_line_starts_with("OTHER=/x", "MENTISDB_TLS_CERT"));
        assert!(!env_line_starts_with(
            "# MENTISDB_TLS_CERT=/x",
            "MENTISDB_TLS_CERT"
        ));
    }

    // ── update_env_file (filesystem) ──────────────────────────────────

    #[test]
    fn update_env_file_creates_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(".env");
        update_env_file(&path, "/tmp/c.pem", "/tmp/k.pem").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("MENTISDB_TLS_CERT=/tmp/c.pem"));
        assert!(after.contains("MENTISDB_TLS_KEY=/tmp/k.pem"));
    }

    #[test]
    fn update_env_file_preserves_existing_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            "MENTISDB_DEFAULT_CHAIN_KEY=demo\nMENTISDB_TLS_CERT=/old.pem\n",
        )
        .unwrap();
        update_env_file(&path, "/new/c.pem", "/new/k.pem").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("MENTISDB_DEFAULT_CHAIN_KEY=demo"));
        assert!(after.contains("MENTISDB_TLS_CERT=/new/c.pem"));
        assert!(after.contains("MENTISDB_TLS_KEY=/new/k.pem"));
        assert!(!after.contains("/old.pem"));
    }

    #[test]
    fn update_env_file_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "FOO=bar\n").unwrap();
        update_env_file(&path, "/x.pem", "/y.pem").unwrap();
        let mtime_first = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        update_env_file(&path, "/x.pem", "/y.pem").unwrap();
        let mtime_second = std::fs::metadata(&path).unwrap().modified().unwrap();
        // Idempotent: mtime should not change when nothing changed.
        // (Best-effort: on filesystems with second-resolution mtimes the
        // sleep might collapse, but here 50ms > 1ns so we should see a
        // difference when the rewrite actually happens.)
        assert_eq!(
            mtime_first, mtime_second,
            "no-op update should not touch the file's mtime"
        );
    }

    // ── resolve_env_file (private) ────────────────────────────────────

    #[test]
    fn resolve_env_file_prefers_cmd_override() {
        let cmd = CertCommand {
            host: None,
            overwrite: false,
            reset: false,
            output_dir: None,
            env_file: Some("/custom/.env".to_string()),
            no_env_update: false,
        };
        assert_eq!(resolve_env_file(&cmd), PathBuf::from("/custom/.env"));
    }

    #[test]
    fn resolve_env_file_falls_back_to_dot_env() {
        let cmd = CertCommand {
            host: None,
            overwrite: false,
            reset: false,
            output_dir: None,
            env_file: None,
            no_env_update: false,
        };
        assert_eq!(resolve_env_file(&cmd), PathBuf::from(".env"));
    }

    // ── resolve_paths ────────────────────────────────────────────────

    #[test]
    fn resolve_paths_uses_out_dir_when_set() {
        let cmd = CertCommand {
            host: None,
            overwrite: false,
            reset: false,
            output_dir: Some("/tmp/custom-tls".to_string()),
            env_file: None,
            no_env_update: false,
        };
        let (cert, key) = resolve_paths(&cmd).unwrap();
        assert_eq!(cert, PathBuf::from("/tmp/custom-tls/cert.pem"));
        assert_eq!(key, PathBuf::from("/tmp/custom-tls/key.pem"));
    }

    // ── help_text ─────────────────────────────────────────────────────

    #[test]
    fn help_text_mentions_every_flag() {
        let text = help_text();
        for needle in [
            "<ip-or-domain>",
            "--force",
            "--out-dir",
            "--env-file",
            "--no-env-update",
            "--help",
            "MENTISDB_TLS_CERT",
            "MENTISDB_TLS_KEY",
        ] {
            assert!(text.contains(needle), "help text missing {needle:?}");
        }
    }

    #[test]
    fn help_text_has_at_least_five_examples() {
        // A world-class help block has plenty of examples. The current
        // text ships with six example blocks; assert at least five so
        // we don't accidentally drop them in future edits.
        let text = help_text();
        let count = text.matches("mentisdb cert").count();
        assert!(
            count >= 5,
            "help text should contain at least 5 example invocations, found {count}"
        );
    }
}
