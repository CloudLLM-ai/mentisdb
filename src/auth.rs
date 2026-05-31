//! Bearer-token registry and authorization helpers for MentisDB servers.
//!
//! The registry stores only token hashes on disk. Raw bearer tokens are shown
//! exactly once by token creation flows and cannot be recovered from the
//! persisted registry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Environment variable that controls server-side bearer-token enforcement.
pub const MENTISDB_BEARER_TOKEN_ACCESS_ENV: &str = "MENTISDB_BEARER_TOKEN_ACCESS";

const BEARER_TOKEN_REGISTRY_FILENAME: &str = "bearer-tokens.json";

/// One bearer token entry persisted in the MentisDB token registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BearerTokenRecord {
    /// Human-friendly token alias chosen by the operator.
    pub alias: String,
    /// SHA-256 hash of the raw bearer token.
    pub token_hash: String,
    /// Time when the token was created.
    pub created_at: DateTime<Utc>,
    /// Last successful authorization time, if known.
    pub last_used_at: Option<DateTime<Utc>>,
    /// Time when the token was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl BearerTokenRecord {
    /// Return `true` when this record can authorize incoming requests.
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// Returned when a bearer token is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedBearerToken {
    /// Persisted token record.
    pub record: BearerTokenRecord,
    /// Raw bearer token. This value is never stored by MentisDB.
    pub token: String,
}

/// Errors raised by bearer-token registry operations.
#[derive(Debug)]
pub enum BearerTokenError {
    /// Alias is empty or contains unsupported characters.
    InvalidAlias(String),
    /// Alias already exists in the registry.
    AliasExists(String),
    /// Alias was not found in the registry.
    AliasNotFound(String),
    /// Registry file I/O failed.
    Io(io::Error),
    /// Registry JSON could not be parsed or serialized.
    Json(serde_json::Error),
}

impl fmt::Display for BearerTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAlias(alias) => write!(f, "invalid bearer token alias: {alias}"),
            Self::AliasExists(alias) => write!(f, "bearer token alias already exists: {alias}"),
            Self::AliasNotFound(alias) => write!(f, "bearer token alias not found: {alias}"),
            Self::Io(error) => write!(f, "bearer token registry I/O error: {error}"),
            Self::Json(error) => write!(f, "bearer token registry JSON error: {error}"),
        }
    }
}

impl std::error::Error for BearerTokenError {}

impl From<io::Error> for BearerTokenError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for BearerTokenError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Durable bearer-token registry backed by a JSON file.
#[derive(Debug, Clone)]
pub struct BearerTokenStore {
    path: PathBuf,
}

impl BearerTokenStore {
    /// Create a token store under `mentisdb_dir`.
    pub fn new(mentisdb_dir: impl AsRef<Path>) -> Self {
        Self {
            path: mentisdb_dir.as_ref().join(BEARER_TOKEN_REGISTRY_FILENAME),
        }
    }

    /// Return the registry file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a new active bearer token for `alias`.
    ///
    /// The returned raw token is shown once and is never persisted.
    pub fn create(&self, alias: &str) -> Result<CreatedBearerToken, BearerTokenError> {
        validate_alias(alias)?;
        let mut records = self.load_records()?;
        if records.iter().any(|record| record.alias == alias) {
            return Err(BearerTokenError::AliasExists(alias.to_string()));
        }

        let token = generate_token();
        let record = BearerTokenRecord {
            alias: alias.to_string(),
            token_hash: hash_token(&token),
            created_at: Utc::now(),
            last_used_at: None,
            revoked_at: None,
        };
        records.push(record.clone());
        self.save_records(&records)?;

        Ok(CreatedBearerToken { record, token })
    }

    /// List all known bearer token records.
    pub fn list(&self) -> Result<Vec<BearerTokenRecord>, BearerTokenError> {
        self.load_records()
    }

    /// Revoke the bearer token identified by `alias`.
    pub fn revoke(&self, alias: &str) -> Result<BearerTokenRecord, BearerTokenError> {
        validate_alias(alias)?;
        let mut records = self.load_records()?;
        let now = Utc::now();
        let Some(index) = records.iter().position(|record| record.alias == alias) else {
            return Err(BearerTokenError::AliasNotFound(alias.to_string()));
        };
        records[index].revoked_at = Some(now);
        let record = records[index].clone();
        self.save_records(&records)?;
        Ok(record)
    }

    /// Return `true` when `token` matches an active record.
    pub fn authorize(&self, token: &str) -> bool {
        let Ok(mut records) = self.load_records() else {
            return false;
        };
        let token_hash = hash_token(token);
        let Some(index) = records.iter().position(|record| {
            record.is_active()
                && record
                    .token_hash
                    .as_bytes()
                    .ct_eq(token_hash.as_bytes())
                    .into()
        }) else {
            return false;
        };
        records[index].last_used_at = Some(Utc::now());
        let _ = self.save_records(&records);
        true
    }

    fn load_records(&self) -> Result<Vec<BearerTokenRecord>, BearerTokenError> {
        match fs::read_to_string(&self.path) {
            Ok(content) => serde_json::from_str(&content).map_err(BearerTokenError::from),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(BearerTokenError::Io(error)),
        }
    }

    fn save_records(&self, records: &[BearerTokenRecord]) -> Result<(), BearerTokenError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = self.path.with_extension("json.tmp");
        let content = serde_json::to_string_pretty(records)?;
        fs::write(&temp_path, content)?;
        fs::rename(temp_path, &self.path)?;
        Ok(())
    }
}

/// Parse whether bearer-token access is enabled from an environment-style value.
pub fn parse_bearer_token_access(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Return whether bearer-token access is enabled by environment.
pub fn bearer_token_access_from_env() -> bool {
    std::env::var(MENTISDB_BEARER_TOKEN_ACCESS_ENV)
        .ok()
        .is_some_and(|value| parse_bearer_token_access(&value))
}

fn validate_alias(alias: &str) -> Result<(), BearerTokenError> {
    let valid = !alias.is_empty()
        && alias
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(BearerTokenError::InvalidAlias(alias.to_string()))
    }
}

fn generate_token() -> String {
    format!(
        "mdb_live_{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

fn hash_token(token: &str) -> String {
    hex_encode(&Sha256::digest(token.as_bytes()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "mentisdb_auth_test_{}_{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    /// Created tokens authorize while the registry stores only their hash.
    #[test]
    fn created_token_authorizes_without_persisting_plaintext() {
        let dir = unique_dir();
        let store = BearerTokenStore::new(&dir);
        let created = store.create("codex").unwrap();

        assert!(store.authorize(&created.token));
        assert!(!store.authorize("wrong"));

        let raw_file = fs::read_to_string(store.path()).unwrap();
        assert!(!raw_file.contains(&created.token));

        let _ = fs::remove_dir_all(dir);
    }

    /// Revoked tokens no longer authorize.
    #[test]
    fn revoked_token_does_not_authorize() {
        let dir = unique_dir();
        let store = BearerTokenStore::new(&dir);
        let created = store.create("codex").unwrap();

        assert!(store.authorize(&created.token));
        store.revoke("codex").unwrap();
        assert!(!store.authorize(&created.token));

        let _ = fs::remove_dir_all(dir);
    }

    /// Aliases reject empty strings and shell-hostile characters.
    #[test]
    fn aliases_are_validated() {
        let dir = unique_dir();
        let store = BearerTokenStore::new(&dir);

        assert!(store.create("codex.laptop-1").is_ok());
        assert!(matches!(
            store.create("bad alias"),
            Err(BearerTokenError::InvalidAlias(_))
        ));
        assert!(matches!(
            store.create(""),
            Err(BearerTokenError::InvalidAlias(_))
        ));

        let _ = fs::remove_dir_all(dir);
    }
}
