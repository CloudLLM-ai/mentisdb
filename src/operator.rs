//! Operator authority: the only way to change who may write to, and read, a chain.
//!
//! Once a chain is private, the list of keys allowed on it, the registry of
//! agent keys and the status of an agent are administered by the **operator**
//! and by nobody else. The operator is whoever can reach one of two places:
//!
//! * the **dashboard**, behind its PIN, and
//! * the **local `mentisdb` command line**, which reads the operator key from
//!   the data directory (filesystem access to the data directory is the
//!   boundary: whoever has it already holds every byte of every chain).
//!
//! REST and MCP, with any bearer token, global or chain-scoped, cannot do any
//! of this. A stolen token, a prompt-injected agent or a hostile web page can
//! append (when the chain lets them) and read, never grant themselves a key.
//!
//! # How a change is authorized
//!
//! Policy records are signed (see [`crate::access`]). The operator signs them
//! with an Ed25519 key kept in `<data dir>/operator/operator.key` (owner-only
//! permissions, created on first use). That key is listed as a governor in every
//! policy record the operator writes and cannot be removed by [`OperatorOp::DenyKey`],
//! so the operator can always take the chain back: it is the recovery path for
//! a lost agent key, and a policy can never lock out its own operator.
//!
//! Records signed by an agent's own governor key (the library function
//! [`crate::MentisDb::set_access_policy`]) stay valid, which is how chains
//! governed before this model keep working: adopt the operator key onto such a
//! chain once with the old governor's signature and the chain is operator
//! managed from then on.
//!
//! The operator key never leaves the host and is never sent anywhere.
//! Back up `operator/operator.key` with the chains: without it, and without a
//! governor key, a private chain's key list can no longer be changed.
//!
//! # Examples
//!
//! ```
//! use mentisdb::access::AccessMode;
//! use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
//! use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};
//!
//! let dir = std::env::temp_dir().join(format!("mentisdb_doc_operator_{}", std::process::id()));
//! let operator = OperatorIdentity::load_or_create(&dir).unwrap();
//! let mut chain = MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary).unwrap();
//! chain.append("alice", ThoughtType::Insight, "public note").unwrap();
//!
//! // Make the chain private: from here on writes need an authorized key.
//! apply(&mut chain, &operator, &OperatorOp::SetMode { mode: AccessMode::Allowlist, keys: vec![] }).unwrap();
//! assert!(chain.append("alice", ThoughtType::Insight, "unsigned").is_err());
//! assert!(chain.segments().has_private());
//! # let _ = std::fs::remove_dir_all(&dir);
//! ```

use crate::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use crate::signing::{
    from_hex, key_registration_payload, thought_payload_v2, to_hex, validate_public_key,
    StoredSignature,
};
use crate::{AgentStatus, MentisDb, PublicKeyAlgorithm};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// Agent id under which the operator signs governance records on every chain.
pub const OPERATOR_AGENT_ID: &str = "mentisdb-operator";
/// Directory (inside the data directory) holding the operator key.
pub const OPERATOR_DIR: &str = "operator";
/// File name of the operator key (64 hex characters of seed and a newline).
pub const OPERATOR_KEY_FILE: &str = "operator.key";

/// The operator's signing key.
///
/// # Examples
///
/// ```
/// use mentisdb::operator::OperatorIdentity;
///
/// let dir = std::env::temp_dir().join(format!("mentisdb_doc_identity_{}", std::process::id()));
/// let first = OperatorIdentity::load_or_create(&dir).unwrap();
/// let again = OperatorIdentity::load_or_create(&dir).unwrap();
/// assert_eq!(first.public_key(), again.public_key());
/// assert!(first.key_id().starts_with("op-"));
/// # let _ = std::fs::remove_dir_all(&dir);
/// ```
pub struct OperatorIdentity {
    signing: SigningKey,
}

impl std::fmt::Debug for OperatorIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OperatorIdentity({})", self.key_id())
    }
}

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

impl OperatorIdentity {
    /// Path of the key file under `data_dir`.
    #[must_use]
    pub fn key_path(data_dir: &Path) -> PathBuf {
        data_dir.join(OPERATOR_DIR).join(OPERATOR_KEY_FILE)
    }

    /// Load the operator key from `data_dir`, creating it on first use.
    ///
    /// The directory is created owner-only (`0700`) and the key file with
    /// `0600`, never overwritten, and created atomically so two processes
    /// racing at first start end up with one key.
    ///
    /// # Errors
    ///
    /// I/O errors, or `InvalidData` when an existing file is not a 32-byte hex seed.
    pub fn load_or_create(data_dir: &Path) -> io::Result<Self> {
        let path = Self::key_path(data_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                writeln!(file, "{}", to_hex(&fresh_seed()))?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        Self::load(&path)
    }

    /// Load an existing operator key file.
    ///
    /// # Errors
    ///
    /// I/O errors, or `InvalidData` when the file is not a 32-byte hex seed.
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let seed: [u8; 32] = from_hex(text.trim())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} must hold a 32-byte seed as 64 hex characters",
                        path.display()
                    ),
                )
            })?;
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// Sign `message` with the operator key (64-byte signature).
    #[must_use]
    pub fn sign_bytes(&self, message: &[u8]) -> [u8; 64] {
        self.signing.sign(message).to_bytes()
    }

    /// The operator's public key.
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    /// Key id used in registries and policy records: `op-` and the first 8 hex
    /// characters of the public key.
    #[must_use]
    pub fn key_id(&self) -> String {
        format!("op-{}", &to_hex(&self.public_key())[..8])
    }

    /// The operator's entry in a policy key set.
    #[must_use]
    pub fn allowed_key(&self) -> AllowedKey {
        AllowedKey::new(OPERATOR_AGENT_ID, &self.key_id(), &self.public_key())
    }
}

/// An advisory lock on a data directory, held while a process owns its chains.
///
/// The daemon (and any in-process service) takes it when it starts; the
/// `--offline` operator commands refuse to run while it is held, because two
/// processes must not append to one chain file. Dropping the value releases it
/// (the operating system also releases it if the process dies).
///
/// # Examples
///
/// ```
/// use mentisdb::operator::DataDirLock;
///
/// let dir = std::env::temp_dir().join(format!("mentisdb_doc_lock_{}", std::process::id()));
/// let first = DataDirLock::try_acquire(&dir).unwrap().expect("free");
/// assert!(DataDirLock::try_acquire(&dir).unwrap().is_none(), "held");
/// drop(first);
/// assert!(DataDirLock::try_acquire(&dir).unwrap().is_some(), "released");
/// # let _ = std::fs::remove_dir_all(&dir);
/// ```
#[derive(Debug)]
pub struct DataDirLock {
    _file: std::fs::File,
}

impl DataDirLock {
    /// Try to take the lock on `<data_dir>/mentisdb.lock`, creating the file.
    ///
    /// Returns `Ok(None)` when another holder has it.
    ///
    /// # Errors
    ///
    /// I/O errors creating or locking the file.
    pub fn try_acquire(data_dir: &Path) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(data_dir)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(data_dir.join("mentisdb.lock"))?;
        match file.try_lock() {
            Ok(()) => {
                // Name the holder for whoever is refused later. Best effort: the
                // lock itself is the authority, the PID is only a courtesy.
                use std::io::Write;
                let _ = file.set_len(0);
                let _ = write!(file, "{}", std::process::id());
                Ok(Some(Self { _file: file }))
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// PID of the process that holds the lock, when it recorded one.
    #[must_use]
    pub fn holder(data_dir: &Path) -> Option<u32> {
        std::fs::read_to_string(data_dir.join("mentisdb.lock"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Take the lock or explain who has it.
    ///
    /// This is what the daemon and every command that writes chain files
    /// directly (`--offline`, `restore`) use: two writers on one hash chain
    /// corrupt it, so the second one refuses instead of waiting or sharing.
    /// The lock is an operating-system advisory lock on the open file, so it is
    /// released when the holder exits or is killed, with no stale lock file to
    /// clean up.
    ///
    /// # Errors
    ///
    /// `WouldBlock` naming the data directory and, when known, the holder's PID;
    /// other I/O errors from creating or locking the file.
    pub fn claim(data_dir: &Path) -> io::Result<Self> {
        Self::try_acquire(data_dir)?.ok_or_else(|| {
            let holder = Self::holder(data_dir)
                .map_or_else(String::new, |pid| format!(" (pid {pid})"));
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "another mentisdb process{holder} is using {}: two processes must not write the same chains. Stop it, use its dashboard route, or choose another MENTISDB_DIR",
                    data_dir.display()
                ),
            )
        })
    }
}

/// A key named by an operator command: agent, key id and public key (hex).
///
/// # Examples
///
/// ```
/// use mentisdb::operator::KeySpec;
///
/// assert!("builder:builder-1:".parse::<KeySpec>().is_err());
/// let ok = format!("builder:builder-1:{}", "ab".repeat(32)).parse::<KeySpec>().unwrap();
/// assert_eq!(ok.agent_id, "builder");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeySpec {
    /// Agent that owns the key.
    pub agent_id: String,
    /// Key id within the agent.
    pub key_id: String,
    /// Ed25519 public key as 64 hex characters.
    pub public_key: String,
}

impl KeySpec {
    fn bytes(&self) -> io::Result<[u8; 32]> {
        check_label("agent_id", &self.agent_id)?;
        check_label("key_id", &self.key_id)?;
        let raw = from_hex(&self.public_key)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        validate_public_key(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
    }
}

impl<'de> Deserialize<'de> for KeySpec {
    /// Accepts the object form or the `AGENT:KEY_ID:HEX` string form.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Text(String),
            Object {
                agent_id: String,
                key_id: String,
                public_key: String,
            },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Text(text) => text.parse().map_err(serde::de::Error::custom),
            Wire::Object {
                agent_id,
                key_id,
                public_key,
            } => Ok(Self {
                agent_id,
                key_id,
                public_key,
            }),
        }
    }
}

impl std::str::FromStr for KeySpec {
    type Err = String;

    /// Parse `AGENT:KEY_ID:PUBLIC_KEY_HEX`.
    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        let mut parts = spec.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(agent), Some(key), Some(public))
                if !agent.trim().is_empty() && !key.trim().is_empty() && public.len() == 64 =>
            {
                Ok(Self {
                    agent_id: agent.trim().to_string(),
                    key_id: key.trim().to_string(),
                    public_key: public.to_ascii_lowercase(),
                })
            }
            _ => Err(format!(
                "expected AGENT:KEY_ID:PUBLIC_KEY_HEX (64 hex characters), got '{spec}'"
            )),
        }
    }
}

/// One administrative change. Serialized as `{"op": "...", ...}`; this is the
/// body the dashboard route and the CLI use.
///
/// # Examples
///
/// ```
/// use mentisdb::operator::OperatorOp;
///
/// let op: OperatorOp = serde_json::from_str(r#"{"op":"deny_key","agent_id":"a","key_id":"k"}"#).unwrap();
/// assert_eq!(op, OperatorOp::DenyKey { agent_id: "a".into(), key_id: "k".into() });
/// assert!(serde_json::from_str::<OperatorOp>(r#"{"op":"sudo"}"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorOp {
    /// Report the chain's access state; changes nothing.
    Show,
    /// Switch between public (`open`) and private (`allowlist`; `signed` is the
    /// legacy any-registered-key mode). Going private on a chain with no policy
    /// starts governance with the operator key (plus `keys`) as the first
    /// governors and opens a private segment; going public closes it.
    SetMode {
        /// New mode; `public` and `private` are accepted as names.
        mode: AccessMode,
        /// Further keys to allow (they are also registered).
        #[serde(default)]
        keys: Vec<KeySpec>,
    },
    /// Allow a key to write to, and read, the chain's private segments.
    AllowKey {
        /// Agent that owns the key.
        agent_id: String,
        /// Key id within the agent.
        key_id: String,
        /// Ed25519 public key as 64 hex characters.
        public_key: String,
    },
    /// Remove a key from the allowed set (the operator key cannot be removed).
    DenyKey {
        /// Agent that owns the key.
        agent_id: String,
        /// Key id within the agent.
        key_id: String,
    },
    /// Register an agent key in the chain's registry without allowing it.
    RegisterKey {
        /// Agent that owns the key.
        agent_id: String,
        /// Key id within the agent.
        key_id: String,
        /// Ed25519 public key as 64 hex characters.
        public_key: String,
    },
    /// Revoke an agent key (older signatures stay valid; the key stops
    /// authorizing reads and writes at once).
    RevokeKey {
        /// Agent that owns the key.
        agent_id: String,
        /// Key id within the agent.
        key_id: String,
    },
    /// Disable an agent: none of its keys authorize anything.
    DisableAgent {
        /// Agent to disable.
        agent_id: String,
    },
    /// Re-enable a disabled agent.
    EnableAgent {
        /// Agent to enable.
        agent_id: String,
    },
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Longest agent id or key id an operator command accepts (bytes). Matches the
/// limit of the read-proof header, so every registered identity can sign a read.
pub const MAX_LABEL_LEN: usize = crate::read_auth::MAX_READ_AUTH_ID_LEN;

/// Ids are labels: non-empty, bounded and free of control characters, because
/// they end up in registries, policy records and HTTP headers.
fn check_label(what: &str, value: &str) -> io::Result<()> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{what} must not be empty")));
    }
    if value.len() > MAX_LABEL_LEN || value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{what} must be at most {MAX_LABEL_LEN} bytes and contain no control characters"
        )));
    }
    Ok(())
}

/// Make sure the operator agent and key exist in the chain's registry, so the
/// operator's signatures verify. The operator proves possession of its own key.
fn ensure_operator_registered(chain: &mut MentisDb, operator: &OperatorIdentity) -> io::Result<()> {
    let key_id = operator.key_id();
    let registered = chain
        .agent_registry()
        .agents
        .get(OPERATOR_AGENT_ID)
        .is_some_and(|agent| agent.public_keys.iter().any(|k| k.key_id == key_id));
    if registered {
        return Ok(());
    }
    chain.upsert_agent(
        OPERATOR_AGENT_ID,
        Some("MentisDB operator"),
        None,
        Some("Signs access-policy records on behalf of the operator"),
        Some(AgentStatus::Active),
    )?;
    let now = chrono::Utc::now().timestamp();
    let public = operator.public_key();
    let payload =
        key_registration_payload(chain.chain_key(), OPERATOR_AGENT_ID, &key_id, &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: operator.signing.sign(&payload).to_bytes().to_vec(),
    };
    chain.add_agent_key(
        OPERATOR_AGENT_ID,
        &key_id,
        PublicKeyAlgorithm::Ed25519,
        public.to_vec(),
        &proof,
    )?;
    Ok(())
}

/// Sign and commit the next policy record.
fn commit_policy(
    chain: &mut MentisDb,
    operator: &OperatorIdentity,
    mode: AccessMode,
    keys: Vec<AllowedKey>,
) -> io::Result<()> {
    ensure_operator_registered(chain, operator)?;
    let seq = chain.access_policy().seq.map_or(0, |seq| seq + 1);
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let record =
        PolicyRecord::new(seq, mode, keys, &prev).map_err(crate::access::AccessError::into_io)?;
    let head = chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default();
    let payload = thought_payload_v2(
        chain.chain_key(),
        OPERATOR_AGENT_ID,
        &head,
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&operator.signing.sign(&payload).to_bytes());
    chain.set_access_policy(OPERATOR_AGENT_ID, &record, &operator.key_id(), signature)?;
    Ok(())
}

fn upsert_key(chain: &mut MentisDb, spec: &KeySpec) -> io::Result<()> {
    chain.add_agent_key_as_operator(
        &spec.agent_id,
        &spec.key_id,
        PublicKeyAlgorithm::Ed25519,
        spec.bytes()?.to_vec(),
    )?;
    Ok(())
}

/// Apply one operator change to `chain` and return what the chain looks like
/// afterwards (`{"changed": bool, "policy": ..., "segments": ...}`).
///
/// This is the single implementation behind the dashboard route and the local
/// CLI; callers are responsible for having authenticated the operator.
///
/// # Errors
///
/// `InvalidInput` for a malformed key, an unknown agent or key, removing the
/// operator key or the last governor, or changing the allowed set of a chain
/// that has never had a policy; other errors come from signing and storage.
///
/// # Examples
///
/// ```
/// use mentisdb::access::AccessMode;
/// use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
/// use mentisdb::{MentisDb, StorageAdapterKind};
///
/// let dir = std::env::temp_dir().join(format!("mentisdb_doc_apply_{}", std::process::id()));
/// let operator = OperatorIdentity::load_or_create(&dir).unwrap();
/// let mut chain = MentisDb::open_with_key_and_storage_kind(&dir, "doc", StorageAdapterKind::Binary).unwrap();
///
/// let key = mentisdb::signing::to_hex(
///     &ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes(),
/// );
/// // The allowed set cannot be edited before the chain has a policy.
/// let allow = OperatorOp::AllowKey { agent_id: "bob".into(), key_id: "bob-1".into(), public_key: key };
/// assert!(apply(&mut chain, &operator, &allow).is_err());
///
/// apply(&mut chain, &operator, &OperatorOp::SetMode { mode: AccessMode::Allowlist, keys: vec![] }).unwrap();
/// let shown = apply(&mut chain, &operator, &allow).unwrap();
/// assert_eq!(shown["changed"], true);
/// // The operator key itself can never be removed.
/// let deny = OperatorOp::DenyKey { agent_id: "mentisdb-operator".into(), key_id: operator.key_id() };
/// assert!(apply(&mut chain, &operator, &deny).is_err());
/// # let _ = std::fs::remove_dir_all(&dir);
/// ```
pub fn apply(
    chain: &mut MentisDb,
    operator: &OperatorIdentity,
    op: &OperatorOp,
) -> io::Result<serde_json::Value> {
    let operator_key = operator.allowed_key();
    let changed = match op {
        OperatorOp::Show => false,
        OperatorOp::SetMode { mode, keys } => {
            if keys.len() >= crate::access::MAX_POLICY_KEYS {
                return Err(invalid(format!(
                    "at most {} keys can be allowed at once",
                    crate::access::MAX_POLICY_KEYS
                )));
            }
            let policy = chain.access_policy().clone();
            for spec in keys {
                upsert_key(chain, spec)?;
            }
            let mut allowed = policy.keys.clone();
            let mut push = |key: AllowedKey| {
                allowed.retain(|k| !(k.agent_id == key.agent_id && k.key_id == key.key_id));
                allowed.push(key);
            };
            push(operator_key.clone());
            for spec in keys {
                push(AllowedKey::new(
                    &spec.agent_id,
                    &spec.key_id,
                    &spec.bytes()?,
                ));
            }
            // A chain with no policy is already public; an identical record is a no-op.
            let already_public = policy.seq.is_none() && *mode == AccessMode::Open;
            let unchanged = policy.seq.is_some() && policy.mode == *mode && policy.keys == allowed;
            if already_public || unchanged {
                false
            } else {
                commit_policy(chain, operator, *mode, allowed)?;
                true
            }
        }
        OperatorOp::AllowKey {
            agent_id,
            key_id,
            public_key,
        } => {
            let spec = KeySpec {
                agent_id: agent_id.clone(),
                key_id: key_id.clone(),
                public_key: public_key.clone(),
            };
            let policy = chain.access_policy().clone();
            if policy.seq.is_none() {
                return Err(invalid(
                    "this chain has no access policy yet; make it private first (set mode private)",
                ));
            }
            upsert_key(chain, &spec)?;
            let entry = AllowedKey::new(agent_id, key_id, &spec.bytes()?);
            if policy.keys.contains(&entry) {
                false
            } else {
                let mut allowed: Vec<AllowedKey> = policy
                    .keys
                    .iter()
                    .filter(|k| !(k.agent_id == *agent_id && k.key_id == *key_id))
                    .cloned()
                    .collect();
                allowed.push(entry);
                commit_policy(chain, operator, policy.mode, allowed)?;
                true
            }
        }
        OperatorOp::DenyKey { agent_id, key_id } => {
            check_label("agent_id", agent_id)?;
            check_label("key_id", key_id)?;
            let policy = chain.access_policy().clone();
            if *agent_id == operator_key.agent_id && *key_id == operator_key.key_id {
                return Err(invalid(
                    "the operator key cannot be removed: it is the chain's recovery path",
                ));
            }
            let allowed: Vec<AllowedKey> = policy
                .keys
                .iter()
                .filter(|k| !(k.agent_id == *agent_id && k.key_id == *key_id))
                .cloned()
                .collect();
            if allowed.len() == policy.keys.len() {
                return Err(invalid(format!(
                    "'{agent_id}/{key_id}' is not in the chain's allowed key set"
                )));
            }
            if allowed.is_empty() {
                return Err(invalid("the allowed key set may not become empty"));
            }
            commit_policy(chain, operator, policy.mode, allowed)?;
            true
        }
        OperatorOp::RegisterKey {
            agent_id,
            key_id,
            public_key,
        } => {
            upsert_key(
                chain,
                &KeySpec {
                    agent_id: agent_id.clone(),
                    key_id: key_id.clone(),
                    public_key: public_key.clone(),
                },
            )?;
            true
        }
        OperatorOp::RevokeKey { agent_id, key_id } => {
            check_label("agent_id", agent_id)?;
            check_label("key_id", key_id)?;
            if agent_id == OPERATOR_AGENT_ID && *key_id == operator.key_id() {
                return Err(invalid("the operator key cannot be revoked"));
            }
            chain.revoke_agent_key(agent_id, key_id)?;
            true
        }
        OperatorOp::DisableAgent { agent_id } => {
            check_label("agent_id", agent_id)?;
            if agent_id == OPERATOR_AGENT_ID {
                return Err(invalid("the operator agent cannot be disabled"));
            }
            chain.disable_agent(agent_id)?;
            true
        }
        OperatorOp::EnableAgent { agent_id } => {
            chain.upsert_agent(agent_id, None, None, None, Some(AgentStatus::Active))?;
            true
        }
    };
    Ok(serde_json::json!({
        "chain_key": chain.chain_key(),
        "changed": changed,
        "policy": chain.access_policy(),
        "operator_key": { "agent_id": OPERATOR_AGENT_ID, "key_id": operator.key_id() },
    }))
}
