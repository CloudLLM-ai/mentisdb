//! Bearer-token registry and authorization helpers for MentisDB MCP servers.
//!
//! The registry stores only token hashes on disk. Raw bearer tokens are shown
//! exactly once by token creation flows and cannot be recovered from the
//! persisted registry.
//!
//! # When to use bearer tokens
//!
//! Local MentisDB daemons usually run with bearer-token access disabled so an
//! MCP client on the same workstation can connect without extra setup. Remote
//! or multi-user daemons should enable
//! [`MENTISDB_BEARER_TOKEN_ACCESS_ENV`] and issue tokens through the
//! `mentisdb bearertoken` CLI or the dashboard Settings screen.
//!
//! Tokens can be global or scoped to one or more chains:
//!
//! - [`BearerTokenScope::Global`] authorizes all chains and server-wide tools.
//! - [`BearerTokenScope::Chains`] authorizes an explicit set of chain keys.
//!
//! # Storage model
//!
//! [`BearerTokenStore`] writes `bearer-tokens.json` under the configured
//! MentisDB directory. Each record contains an alias, SHA-256 token hash,
//! timestamps, revocation state, and scope. Raw tokens are generated and
//! returned once by [`BearerTokenStore::create`]; callers must display or copy
//! that value immediately because it is never persisted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Environment variable that controls server-side bearer-token enforcement.
///
/// Set this to `true`, `1`, `yes`, or `on` to require an active bearer token
/// on MCP requests. The default is disabled for local development.
pub const MENTISDB_BEARER_TOKEN_ACCESS_ENV: &str = "MENTISDB_BEARER_TOKEN_ACCESS";

const BEARER_TOKEN_REGISTRY_FILENAME: &str = "bearer-tokens.json";

/// Authorization scope carried by a bearer token.
///
/// Scope is the durable policy attached to a token record. Global tokens are
/// intended for trusted administrators and automation that may inspect or
/// mutate server-wide state. Chain-scoped tokens are intended for users or
/// agents that should only interact with a specific set of memory chains.
///
/// # String format
///
/// The CLI and dashboard display scopes as:
///
/// - `global`
/// - `chain:<chain_key>`
/// - `chains:<chain_key>,<chain_key>`
///
/// # Examples
///
/// ```
/// use mentisdb::auth::BearerTokenScope;
///
/// let global = BearerTokenScope::Global;
/// assert!(global.allows_chain("any-chain"));
///
/// let scoped = BearerTokenScope::chain("alice")?;
/// assert!(scoped.allows_chain("alice"));
/// assert!(!scoped.allows_chain("bob"));
/// assert_eq!(scoped.to_string(), "chain:alice");
///
/// let team = BearerTokenScope::chains(["alice", "shared"])?;
/// assert!(team.allows_chain("alice"));
/// assert!(team.allows_chain("shared"));
/// assert!(!team.allows_chain("private"));
/// # Ok::<(), mentisdb::auth::BearerTokenError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BearerTokenScope {
    /// Token may authorize requests for any chain.
    #[default]
    Global,
    /// Token may authorize requests for one or more explicit chain keys.
    Chains(Vec<String>),
}

impl BearerTokenScope {
    /// Build a chain-scoped token scope after validating the chain key.
    ///
    /// Chain keys must not be empty or whitespace-only. The method keeps
    /// validation close to scope creation so CLI, dashboard, and server code all
    /// share the same rule.
    ///
    /// # Errors
    ///
    /// Returns [`BearerTokenError::InvalidChainKey`] when `chain_key` is empty.
    pub fn chain(chain_key: impl Into<String>) -> Result<Self, BearerTokenError> {
        Self::chains([chain_key.into()])
    }

    /// Build a chain-scoped token scope from one or more chain keys.
    ///
    /// Duplicate chain keys are collapsed, and keys are sorted to make display
    /// and persisted JSON stable across callers.
    ///
    /// # Errors
    ///
    /// Returns [`BearerTokenError::InvalidChainKey`] when any chain key is
    /// empty, or when the final set contains no chain keys.
    pub fn chains<I, S>(chain_keys: I) -> Result<Self, BearerTokenError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut normalized = BTreeSet::new();
        for chain_key in chain_keys {
            normalized.insert(normalize_chain_key(chain_key)?);
        }
        if normalized.is_empty() {
            return Err(BearerTokenError::InvalidChainKey(String::new()));
        }
        Ok(Self::Chains(normalized.into_iter().collect()))
    }

    /// Return `true` when this scope permits access to `chain_key`.
    ///
    /// Global scopes permit every chain. Chain scopes require exact string
    /// equality with one configured key.
    pub fn allows_chain(&self, chain_key: &str) -> bool {
        match self {
            Self::Global => true,
            Self::Chains(allowed) => allowed.iter().any(|allowed| allowed == chain_key),
        }
    }

    /// Return the explicit chain keys, or `None` for global scopes.
    pub fn chain_keys(&self) -> Option<&[String]> {
        match self {
            Self::Global => None,
            Self::Chains(chain_keys) => Some(chain_keys),
        }
    }

    fn validate(&self) -> Result<(), BearerTokenError> {
        if let Self::Chains(chain_keys) = self {
            let _ = Self::chains(chain_keys.clone())?;
        }
        Ok(())
    }
}

impl fmt::Display for BearerTokenScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => f.write_str("global"),
            Self::Chains(chain_keys) if chain_keys.len() == 1 => {
                write!(f, "chain:{}", chain_keys[0])
            }
            Self::Chains(chain_keys) => write!(f, "chains:{}", chain_keys.join(",")),
        }
    }
}

impl FromStr for BearerTokenScope {
    type Err = BearerTokenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "global" {
            return Ok(Self::Global);
        }
        if let Some(chain_key) = value.strip_prefix("chain:") {
            return Self::chain(chain_key.to_string());
        }
        if let Some(chain_keys) = value.strip_prefix("chains:") {
            return Self::chains(chain_keys.split(',').map(str::to_string));
        }
        Err(BearerTokenError::InvalidScope(value.to_string()))
    }
}

impl Serialize for BearerTokenScope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum ScopeWire<'a> {
            Global,
            Chain { chain_key: &'a str },
            Chains { chain_keys: &'a [String] },
        }

        match self {
            Self::Global => ScopeWire::Global.serialize(serializer),
            Self::Chains(chain_keys) if chain_keys.len() == 1 => ScopeWire::Chain {
                chain_key: &chain_keys[0],
            }
            .serialize(serializer),
            Self::Chains(chain_keys) => ScopeWire::Chains { chain_keys }.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for BearerTokenScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum ScopeWire {
            Global,
            Chain { chain_key: String },
            Chains { chain_keys: Vec<String> },
        }

        match ScopeWire::deserialize(deserializer)? {
            ScopeWire::Global => Ok(Self::Global),
            ScopeWire::Chain { chain_key } => {
                Self::chain(chain_key).map_err(serde::de::Error::custom)
            }
            ScopeWire::Chains { chain_keys } => {
                Self::chains(chain_keys).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// One bearer token entry persisted in the MentisDB token registry.
///
/// Records are safe to list in an admin UI because they do not contain raw
/// bearer tokens. `token_hash` is a SHA-256 hex digest; the only time the raw
/// token exists is the [`CreatedBearerToken::token`] returned by
/// [`BearerTokenStore::create`].
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
    /// Chains this token can authorize.
    ///
    /// Older registries written before scoped tokens omit this field, or store
    /// a single `chain` field. The serde default treats missing scopes as
    /// global, and the scope deserializer still accepts single-chain records so
    /// existing deployments continue to work after upgrade.
    #[serde(default)]
    pub scope: BearerTokenScope,
}

impl BearerTokenRecord {
    /// Return `true` when this record can authorize incoming requests.
    ///
    /// This only checks revocation state. Scope checks are performed by
    /// [`BearerTokenStore::authorize_for_chain`],
    /// [`BearerTokenStore::authorize_for_chains`], or
    /// [`BearerTokenStore::authorize_global`].
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// Returned when a bearer token is created.
///
/// `record` is the metadata persisted to disk. `token` is the raw secret to
/// return to the operator once. Do not log the raw token in long-lived logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedBearerToken {
    /// Persisted token record.
    pub record: BearerTokenRecord,
    /// Raw bearer token. This value is never stored by MentisDB.
    pub token: String,
}

/// Errors raised by bearer-token registry operations.
///
/// The variants are intentionally specific so CLI and dashboard callers can
/// return clear operator-facing messages without string matching.
#[derive(Debug)]
pub enum BearerTokenError {
    /// Alias is empty or contains unsupported characters.
    InvalidAlias(String),
    /// Chain key is empty.
    InvalidChainKey(String),
    /// Token scope string could not be parsed.
    InvalidScope(String),
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
            Self::InvalidChainKey(chain_key) => {
                write!(f, "invalid bearer token chain key: {chain_key}")
            }
            Self::InvalidScope(scope) => write!(f, "invalid bearer token scope: {scope}"),
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
///
/// The store is a small file-backed registry designed for MCP server
/// authentication. It favors operational transparency over database
/// complexity: operators can back up or inspect `bearer-tokens.json`, while
/// authorization still compares token hashes in constant time.
///
/// # Examples
///
/// Create a chain-scoped token and authorize requests against it:
///
/// ```
/// use mentisdb::auth::{BearerTokenScope, BearerTokenStore};
///
/// let dir = std::env::temp_dir().join(format!("mentisdb-auth-doc-{}", uuid::Uuid::new_v4()));
/// let store = BearerTokenStore::new(&dir);
/// let created = store.create("alice-agent", BearerTokenScope::chain("alice")?)?;
///
/// assert!(store.authorize_for_chain(&created.token, "alice"));
/// assert!(!store.authorize_for_chain(&created.token, "bob"));
///
/// store.revoke("alice-agent")?;
/// assert!(!store.authorize_for_chain(&created.token, "alice"));
///
/// let _ = std::fs::remove_dir_all(dir);
/// # Ok::<(), mentisdb::auth::BearerTokenError>(())
/// ```
#[derive(Debug, Clone)]
pub struct BearerTokenStore {
    path: PathBuf,
    /// File-level lock to prevent concurrent read-modify-write races
    /// between `create`, `revoke`, and `authorize_matching` calls.
    /// The lock is per-store instance; clones share the same lock.
    lock: Arc<std::sync::Mutex<()>>,
}

impl BearerTokenStore {
    /// Create a token store under `mentisdb_dir`.
    ///
    /// The registry file is not created until the first mutating operation.
    /// This makes it cheap to construct a store during server startup even when
    /// bearer-token access is disabled.
    pub fn new(mentisdb_dir: impl AsRef<Path>) -> Self {
        Self {
            path: mentisdb_dir.as_ref().join(BEARER_TOKEN_REGISTRY_FILENAME),
            lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Return the registry file path.
    ///
    /// The file name is always `bearer-tokens.json` under the MentisDB storage
    /// directory supplied to [`BearerTokenStore::new`].
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create a new active bearer token for `alias`.
    ///
    /// The returned raw token is shown once and is never persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    ///
    /// - `alias` is empty or contains unsupported characters.
    /// - `scope` carries an invalid chain key.
    /// - another token already uses `alias`.
    /// - the registry cannot be read or written.
    ///
    /// # Examples
    ///
    /// ```
    /// use mentisdb::auth::{BearerTokenScope, BearerTokenStore};
    ///
    /// let dir = std::env::temp_dir().join(format!("mentisdb-token-doc-{}", uuid::Uuid::new_v4()));
    /// let store = BearerTokenStore::new(&dir);
    /// let created = store.create("admin", BearerTokenScope::Global)?;
    ///
    /// assert_eq!(created.record.alias, "admin");
    /// assert_eq!(created.record.scope, BearerTokenScope::Global);
    /// assert!(created.token.starts_with("mentisdb_"));
    ///
    /// let _ = std::fs::remove_dir_all(dir);
    /// # Ok::<(), mentisdb::auth::BearerTokenError>(())
    /// ```
    pub fn create(
        &self,
        alias: &str,
        scope: BearerTokenScope,
    ) -> Result<CreatedBearerToken, BearerTokenError> {
        validate_alias(alias)?;
        scope.validate()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
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
            scope,
        };
        records.push(record.clone());
        self.save_records(&records)?;

        Ok(CreatedBearerToken { record, token })
    }

    /// List all known bearer token records.
    ///
    /// The returned records include aliases, scopes, timestamps, and hashes,
    /// but never raw bearer tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read or parsed.
    pub fn list(&self) -> Result<Vec<BearerTokenRecord>, BearerTokenError> {
        self.load_records()
    }

    /// Revoke the bearer token identified by `alias`.
    ///
    /// Revocation is durable: the record remains in the registry with
    /// `revoked_at` set, and subsequent authorization attempts fail.
    ///
    /// # Errors
    ///
    /// Returns [`BearerTokenError::AliasNotFound`] when no record exists for
    /// `alias`, or an I/O/JSON error when the registry cannot be updated.
    pub fn revoke(&self, alias: &str) -> Result<BearerTokenRecord, BearerTokenError> {
        validate_alias(alias)?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
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

    /// Return `true` when `token` matches any active record.
    ///
    /// This method intentionally ignores scope. It is useful for MCP handshake
    /// or metadata operations that require a valid token but do not touch chain
    /// data. Chain-aware request paths should prefer
    /// [`BearerTokenStore::authorize_for_chain`],
    /// [`BearerTokenStore::authorize_for_chains`], or
    /// [`BearerTokenStore::authorize_global`].
    pub fn authorize(&self, token: &str) -> bool {
        self.authorize_matching(token, |_| true)
    }

    /// Return `true` when `token` matches an active record that can access `chain_key`.
    ///
    /// Global tokens always pass. Chain-scoped tokens pass only when
    /// `chain_key` exactly matches the configured chain key.
    pub fn authorize_for_chain(&self, token: &str, chain_key: &str) -> bool {
        self.authorize_matching(token, |record| record.scope.allows_chain(chain_key))
    }

    /// Return `true` when `token` matches an active record that can access every
    /// requested chain.
    ///
    /// Use this for MCP tools whose arguments can mention multiple chains, such
    /// as federated search, chain merges, branch creation, or relation payloads
    /// that reference thoughts in another chain.
    pub fn authorize_for_chains(&self, token: &str, chain_keys: &[String]) -> bool {
        self.authorize_matching(token, |record| {
            chain_keys
                .iter()
                .all(|chain_key| record.scope.allows_chain(chain_key))
        })
    }

    /// Return `true` when `token` matches an active global record.
    ///
    /// Use this for server-wide operations such as listing all chains or
    /// managing global webhook state. Chain-scoped tokens always fail this
    /// check, even if their configured chain is the default chain.
    pub fn authorize_global(&self, token: &str) -> bool {
        self.authorize_matching(token, |record| {
            matches!(record.scope, BearerTokenScope::Global)
        })
    }

    fn authorize_matching(
        &self,
        token: &str,
        matches_scope: impl Fn(&BearerTokenRecord) -> bool,
    ) -> bool {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(mut records) = self.load_records() else {
            return false;
        };
        let token_hash = hash_token(token);
        let Some(index) = records.iter().position(|record| {
            record.is_active()
                && matches_scope(record)
                && record
                    .token_hash
                    .as_bytes()
                    .ct_eq(token_hash.as_bytes())
                    .into()
        }) else {
            return false;
        };
        records[index].last_used_at = Some(Utc::now());
        if let Err(e) = self.save_records(&records) {
            eprintln!("[mentisdb] failed to persist last_used_at for bearer token: {e}");
        }
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
///
/// Accepted truthy values are `1`, `true`, `yes`, and `on`, matched
/// case-insensitively after trimming whitespace. Any other value is false.
pub fn parse_bearer_token_access(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Return whether bearer-token access is enabled by environment.
///
/// This reads [`MENTISDB_BEARER_TOKEN_ACCESS_ENV`] from the process
/// environment. Missing or unrecognized values disable enforcement.
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

fn validate_chain_key(chain_key: &str) -> Result<(), BearerTokenError> {
    if chain_key.trim().is_empty() {
        Err(BearerTokenError::InvalidChainKey(chain_key.to_string()))
    } else {
        Ok(())
    }
}

fn normalize_chain_key(chain_key: impl Into<String>) -> Result<String, BearerTokenError> {
    let chain_key = chain_key.into();
    validate_chain_key(&chain_key)?;
    Ok(chain_key.trim().to_string())
}

fn generate_token() -> String {
    format!(
        "mentisdb_{}{}{}",
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
        let created = store.create("codex", BearerTokenScope::Global).unwrap();

        assert!(store.authorize(&created.token));
        assert!(store.authorize_for_chain(&created.token, "any-chain"));
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
        let created = store.create("codex", BearerTokenScope::Global).unwrap();

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

        assert!(store
            .create("codex.laptop-1", BearerTokenScope::Global)
            .is_ok());
        assert!(matches!(
            store.create("bad alias", BearerTokenScope::Global),
            Err(BearerTokenError::InvalidAlias(_))
        ));
        assert!(matches!(
            store.create("", BearerTokenScope::Global),
            Err(BearerTokenError::InvalidAlias(_))
        ));

        let _ = fs::remove_dir_all(dir);
    }

    /// Global tokens authorize all chains and server-wide operations.
    #[test]
    fn global_token_authorizes_all_chain_and_global_checks() {
        let dir = unique_dir();
        let store = BearerTokenStore::new(&dir);
        let created = store.create("admin", BearerTokenScope::Global).unwrap();

        assert!(store.authorize_for_chain(&created.token, "alpha"));
        assert!(store.authorize_for_chain(&created.token, "beta"));
        assert!(
            store.authorize_for_chains(&created.token, &["alpha".to_string(), "beta".to_string()])
        );
        assert!(store.authorize_global(&created.token));

        let _ = fs::remove_dir_all(dir);
    }

    /// Chain-scoped tokens authorize only their configured chains.
    #[test]
    fn chain_scoped_token_authorizes_only_matching_chains() {
        let dir = unique_dir();
        let store = BearerTokenStore::new(&dir);
        let created = store
            .create(
                "alice",
                BearerTokenScope::chains(["alice-chain", "shared-chain"]).unwrap(),
            )
            .unwrap();

        assert!(store.authorize(&created.token));
        assert!(store.authorize_for_chain(&created.token, "alice-chain"));
        assert!(store.authorize_for_chain(&created.token, "shared-chain"));
        assert!(store.authorize_for_chains(
            &created.token,
            &["alice-chain".to_string(), "shared-chain".to_string()]
        ));
        assert!(!store.authorize_for_chain(&created.token, "private-chain"));
        assert!(!store.authorize_for_chains(
            &created.token,
            &["alice-chain".to_string(), "private-chain".to_string()]
        ));
        assert!(!store.authorize_global(&created.token));

        let _ = fs::remove_dir_all(dir);
    }

    /// Chain scopes normalize input order, whitespace, and duplicates.
    #[test]
    fn chain_scopes_deduplicate_sort_and_reject_empty_sets() {
        let scope = BearerTokenScope::chains([" shared ", "alice", "shared"]).unwrap();
        assert_eq!(
            scope,
            BearerTokenScope::Chains(vec!["alice".to_string(), "shared".to_string()])
        );
        assert_eq!(scope.to_string(), "chains:alice,shared");

        assert!(matches!(
            BearerTokenScope::chains(Vec::<String>::new()),
            Err(BearerTokenError::InvalidChainKey(_))
        ));
        assert!(matches!(
            BearerTokenScope::chains(["alice", " "]),
            Err(BearerTokenError::InvalidChainKey(_))
        ));
    }

    /// Scope strings round-trip through Display and FromStr.
    #[test]
    fn scopes_roundtrip_through_strings() {
        assert_eq!(
            "global".parse::<BearerTokenScope>().unwrap(),
            BearerTokenScope::Global
        );
        assert_eq!(
            "chain:mentisdb".parse::<BearerTokenScope>().unwrap(),
            BearerTokenScope::Chains(vec!["mentisdb".to_string()])
        );
        assert_eq!(
            "chains:mentisdb,gubatron"
                .parse::<BearerTokenScope>()
                .unwrap(),
            BearerTokenScope::Chains(vec!["gubatron".to_string(), "mentisdb".to_string()])
        );
        assert_eq!(BearerTokenScope::Global.to_string(), "global");
        assert_eq!(
            BearerTokenScope::Chains(vec!["mentisdb".to_string()]).to_string(),
            "chain:mentisdb"
        );
        assert_eq!(
            BearerTokenScope::Chains(vec!["gubatron".to_string(), "mentisdb".to_string()])
                .to_string(),
            "chains:gubatron,mentisdb"
        );
    }

    /// Bearer-token records remain compatible with older registry JSON.
    #[test]
    fn token_records_deserialize_legacy_and_multi_chain_scopes() {
        let legacy_missing_scope = r#"{
            "alias": "old-admin",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null
        }"#;
        let record: BearerTokenRecord = serde_json::from_str(legacy_missing_scope).unwrap();
        assert_eq!(record.scope, BearerTokenScope::Global);

        let legacy_single_chain = r#"{
            "alias": "old-chain",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null,
            "scope": { "type": "chain", "chain_key": "alice" }
        }"#;
        let record: BearerTokenRecord = serde_json::from_str(legacy_single_chain).unwrap();
        assert_eq!(
            record.scope,
            BearerTokenScope::Chains(vec!["alice".to_string()])
        );

        let multi_chain = r#"{
            "alias": "team",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null,
            "scope": { "type": "chains", "chain_keys": ["shared", "alice"] }
        }"#;
        let record: BearerTokenRecord = serde_json::from_str(multi_chain).unwrap();
        assert_eq!(
            record.scope,
            BearerTokenScope::Chains(vec!["alice".to_string(), "shared".to_string()])
        );
    }
}
