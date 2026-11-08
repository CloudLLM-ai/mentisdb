//! `mentisdb key`: generate and inspect the Ed25519 keys used to sign thoughts,
//! register agent keys and govern chain access.
//!
//! Key files hold the 32-byte seed as 64 lowercase hex characters and a newline,
//! the format `--private-key-file` reads. Nothing here talks to a daemon.

use crate::signing::{from_hex, to_hex};
use ed25519_dalek::SigningKey;
use std::io::Write;

/// Help text for `mentisdb key`.
pub const KEY_HELP: &str = "\
mentisdb key - generate and inspect Ed25519 signing keys

Usage:
  mentisdb key generate [--out <file>] [--force]
  mentisdb key show <file>

Commands:
  generate   Create a new key. The private seed is ALWAYS written to a file
             with owner-only permissions (0600) and never printed: a private
             key on standard output ends up in terminal scrollback, shell logs
             and, worse, in the context of any AI agent that runs the command.
             Without --out the file is $MENTISDB_KEY_DIR/<public key prefix>.key
             (default ~/.mentisdb/keys, created 0700). Only the public key and
             the file's path are printed.
  show       Print the public key of a key file.

To create a key AND register its public key for an agent in one step, use
'mentisdb agent key generate <agent> --key-id ID --chain KEY' (same file, same
protection; operator only). 'agent key add' registers a key made here.

Options:
  --out <file>   Where to save the private seed (refuses to overwrite).
  --force        Overwrite an existing --out file. The file is made 0600 before
                 the new seed is written, even if it was more open before.
  -h, --help     Show this help

The seed file is what --private-key-file reads (for 'agent key add',
'thought append' and 'chain access ...'). Keep it secret and never paste it
into a chat or hand it to an AI agent; MentisDB only ever sees the public key
and signatures. Loading a seed file that other users can read prints a warning
with the 'chmod 600' fix.

Examples:
  mentisdb key generate
  mentisdb key generate --out ~/.mentisdb/builder.key
  mentisdb key show ~/.mentisdb/builder.key
";

/// Environment variable naming the directory `mentisdb key generate` writes to
/// when `--out` is not given.
pub const KEY_DIR_ENV: &str = "MENTISDB_KEY_DIR";

/// A parsed `mentisdb key ...` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCommand {
    /// Create a key, optionally saving the seed.
    Generate {
        /// Where to write the seed.
        out: Option<String>,
        /// Overwrite an existing file.
        force: bool,
    },
    /// Print the public key of a seed file.
    Show {
        /// Seed file path.
        file: String,
    },
}

/// Parse the arguments after `key`.
///
/// # Errors
///
/// A message for an unknown subcommand, unknown flag or missing argument.
///
/// # Examples
///
/// ```
/// use mentisdb::cli::key::{parse_key_args, KeyCommand};
///
/// let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
/// assert_eq!(
///     parse_key_args(&args(&["generate", "--out", "k"])).unwrap(),
///     KeyCommand::Generate { out: Some("k".into()), force: false }
/// );
/// assert!(parse_key_args(&args(&["show"])).is_err());
/// ```
pub fn parse_key_args(args: &[String]) -> Result<KeyCommand, String> {
    let mut rest = args.iter();
    match rest.next().map(String::as_str) {
        Some("generate") => {
            let (mut out, mut force) = (None, false);
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--out" => {
                        out = Some(rest.next().ok_or("--out requires a value")?.clone());
                    }
                    "--force" => force = true,
                    other => {
                        return Err(format!(
                            "Unknown flag '{other}' for 'mentisdb key generate'"
                        ))
                    }
                }
            }
            Ok(KeyCommand::Generate { out, force })
        }
        Some("show") => match (rest.next(), rest.next()) {
            (Some(file), None) => Ok(KeyCommand::Show { file: file.clone() }),
            _ => Err("'mentisdb key show' takes exactly one <file>".to_string()),
        },
        other => Err(format!(
            "Unknown subcommand '{}' for 'mentisdb key'; run 'mentisdb key --help'",
            other.unwrap_or("")
        )),
    }
}

/// The directory `mentisdb key generate` writes to without `--out`:
/// `$MENTISDB_KEY_DIR`, else `<home>/.mentisdb/keys`.
///
/// # Errors
///
/// A message when neither the variable nor a home directory is available.
///
/// # Examples
///
/// ```
/// std::env::set_var("MENTISDB_KEY_DIR", "/tmp/mentisdb-doc-keys");
/// assert_eq!(
///     mentisdb::cli::key::default_key_dir().unwrap(),
///     std::path::PathBuf::from("/tmp/mentisdb-doc-keys")
/// );
/// std::env::remove_var("MENTISDB_KEY_DIR");
/// ```
pub fn default_key_dir() -> Result<std::path::PathBuf, String> {
    if let Some(dir) = std::env::var_os(KEY_DIR_ENV).filter(|dir| !dir.is_empty()) {
        return Ok(dir.into());
    }
    let environment = crate::paths::PathEnvironment::capture();
    environment
        .home_dir_for(crate::paths::HostPlatform::current())
        .map(|home| home.join(".mentisdb").join("keys"))
        .ok_or_else(|| format!("no home directory found; pass --out <file> or set {KEY_DIR_ENV}"))
}

fn write_seed(path: &str, seed: &[u8; 32], force: bool) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("'{path}' already exists; pass --force to overwrite it")
        } else {
            format!("cannot write '{path}': {e}")
        }
    })?;
    // `--force` reuses an existing file, whose mode `open` leaves alone: tighten
    // it before the seed goes in (the file is empty until now).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot restrict '{path}': {e}"))?;
    }
    writeln!(file, "{}", to_hex(seed)).map_err(|e| format!("cannot write '{path}': {e}"))
}

/// Save a freshly generated `seed` owner-only and return the path it went to.
///
/// With `out` the file is exactly that path; without it the file is
/// `<key dir>/<first 16 hex characters of the public key>.key` and the
/// directory is created owner-only. An existing file is refused unless `force`.
/// Shared by `mentisdb key generate` and `mentisdb agent key generate`, so both
/// write the same format with the same permissions.
///
/// # Errors
///
/// A message when the directory or file cannot be created, or the file exists
/// and `force` is not set. The seed is never part of the message.
pub(crate) fn save_new_seed(
    out: Option<&str>,
    force: bool,
    seed: &[u8; 32],
    public: &[u8; 32],
) -> Result<String, String> {
    let target = match out {
        Some(path) => path.to_string(),
        None => {
            let dir = default_key_dir()?;
            crate::private_fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create '{}': {e}", dir.display()))?;
            dir.join(format!("{}.key", &to_hex(public)[..16]))
                .to_string_lossy()
                .into_owned()
        }
    };
    write_seed(&target, seed, force)?;
    Ok(target)
}

/// Execute a `key` command, printing to `out`.
///
/// # Errors
///
/// A message when the file cannot be written or read, or is not a valid seed.
pub fn run_key(command: &KeyCommand, out: &mut dyn Write) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    match command {
        KeyCommand::Generate { out: path, force } => {
            let seed =
                crate::signing::fresh_seed().map_err(|e| format!("cannot generate a key: {e}"))?;
            let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
            // The seed is never printed: stdout lands in terminal scrollback,
            // logs and the context of any agent that ran this command.
            let target = save_new_seed(path.as_deref(), *force, &seed, &public)?;
            writeln!(out, "public_key_hex: {}", to_hex(&public)).map_err(io)?;
            writeln!(
                out,
                "private key saved to {target} (owner-only; keep it secret, never paste it into a chat or give it to an AI agent)"
            )
            .map_err(io)
        }
        KeyCommand::Show { file } => {
            let text =
                std::fs::read_to_string(file).map_err(|e| format!("cannot read '{file}': {e}"))?;
            let seed: [u8; 32] = from_hex(&text)
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| format!("'{file}' must hold exactly 32 bytes (64 hex characters)"))?;
            let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
            writeln!(out, "public_key_hex: {}", to_hex(&public)).map_err(io)
        }
    }
}
