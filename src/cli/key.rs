//! `mentisdb key`: generate and inspect the Ed25519 keys used to sign thoughts,
//! register agent keys and govern chain access.
//!
//! Key files hold the 32-byte seed as 64 lowercase hex characters and a newline,
//! the format `--private-key-file` reads. Nothing here talks to a daemon.

use crate::signing::{from_hex, to_hex};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use std::io::Write;

/// Help text for `mentisdb key`.
pub const KEY_HELP: &str = "\
mentisdb key - generate and inspect Ed25519 signing keys

Usage:
  mentisdb key generate [--out <file>] [--force]
  mentisdb key show <file>

Commands:
  generate   Create a new key. The private seed is written to --out (default:
             not saved, printed once on standard output) with owner-only
             permissions; the public key is printed as 64 hex characters.
  show       Print the public key of a key file.

Options:
  --out <file>   Where to save the private seed (refuses to overwrite).
  --force        Overwrite an existing --out file.
  -h, --help     Show this help

The seed file is what --private-key-file reads (for 'agent key add',
'thought append' and 'chain access ...'). Keep it secret; MentisDB only ever
sees the public key and signatures.

Examples:
  mentisdb key generate --out ~/.mentisdb/builder.key
  mentisdb key show ~/.mentisdb/builder.key
";

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

/// A fresh 32-byte seed: SHA-256 over two random UUIDs and the clock.
///
/// UUIDv4 draws from the operating system's CSPRNG, so the seed carries at
/// least 240 bits of entropy.
fn fresh_seed() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(uuid::Uuid::new_v4().as_bytes());
    hasher.update(uuid::Uuid::new_v4().as_bytes());
    hasher.update(
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_be_bytes(),
    );
    hasher.finalize().into()
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
    writeln!(file, "{}", to_hex(seed)).map_err(|e| format!("cannot write '{path}': {e}"))
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
            let seed = fresh_seed();
            let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
            writeln!(out, "public_key_hex: {}", to_hex(&public)).map_err(io)?;
            match path {
                Some(path) => {
                    write_seed(path, &seed, *force)?;
                    writeln!(out, "private key saved to {path} (keep it secret)").map_err(io)
                }
                None => writeln!(
                    out,
                    "private_key_seed_hex: {} (shown once; save it)",
                    to_hex(&seed)
                )
                .map_err(io),
            }
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
