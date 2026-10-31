//! Write-access control for chains: typed errors, key-registration proofs and
//! (in later slices) policies and governance records.
//!
//! See `docs/CHAIN_ACCESS.md` for the design. Every rejection is an
//! [`AccessError`]; library functions return it wrapped in [`std::io::Error`]
//! (kind `PermissionDenied`, or `InvalidInput` for malformed input) so existing
//! `io::Result` signatures keep working and callers can recover the typed error
//! with [`AccessError::from_io`].
//!
//! # Examples
//!
//! ```
//! use mentisdb::access::AccessError;
//!
//! let io_error = AccessError::SignatureRequired.into_io();
//! assert_eq!(io_error.kind(), std::io::ErrorKind::PermissionDenied);
//! assert_eq!(AccessError::from_io(&io_error), Some(&AccessError::SignatureRequired));
//! ```

use crate::signing::{
    access_key_leaf, from_hex, merkle_root, thought_payload_v2_stored, to_hex,
    verify_ed25519_signature, SignatureVersion, SigningError, StoredSignature,
};
use crate::{Thought, ThoughtInput, ThoughtRole, ThoughtType};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::str::FromStr;
use uuid::Uuid;

/// Why a write, key registration or policy change was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessError {
    /// The public key is malformed (length, curve point or weak point).
    InvalidPublicKey(SigningError),
    /// The key-registration timestamp is outside the accepted window.
    KeyProofStale {
        /// Seconds between the proof timestamp and the server clock.
        skew_secs: i64,
    },
    /// The key-registration proof of possession does not verify.
    KeyProofInvalid(SigningError),
    /// The thought type is reserved for governance records.
    ReservedThoughtType,
    /// The chain's policy requires a signature and the thought has none.
    SignatureRequired,
    /// A signature is present but `signing_key_id` is not.
    MissingSigningKeyId,
    /// The stored signature has an unknown shape.
    UnsupportedSignature(SigningError),
    /// A v1 signature is not accepted by the chain's policy.
    SignatureVersionNotAllowed(SignatureVersion),
    /// The claimed agent is not registered.
    UnknownAgent(String),
    /// The agent has no key with that id.
    UnknownKey {
        /// Agent that was named.
        agent_id: String,
        /// Key id that was named.
        key_id: String,
    },
    /// The key exists but has been revoked.
    KeyRevoked {
        /// Agent that was named.
        agent_id: String,
        /// Key id that was named.
        key_id: String,
    },
    /// The signature does not verify; for v2 this also happens when the chain
    /// head moved since the signer fetched it (sign again against the new head).
    BadSignature(SigningError),
    /// The key is valid but not in the chain's allowed set.
    KeyNotAllowed {
        /// Agent that was named.
        agent_id: String,
        /// Key id that was named.
        key_id: String,
    },
    /// A policy record is malformed or inconsistent.
    PolicyInvalid(String),
    /// The signer may not change the policy.
    NotAuthorized(String),
}

impl AccessError {
    /// The `io::ErrorKind` this error maps to.
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::InvalidPublicKey(_)
            | Self::KeyProofStale { .. }
            | Self::UnsupportedSignature(_)
            | Self::MissingSigningKeyId
            | Self::PolicyInvalid(_) => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::PermissionDenied,
        }
    }

    /// Wrap into an `io::Error` that [`AccessError::from_io`] can unwrap.
    #[must_use]
    pub fn into_io(self) -> io::Error {
        io::Error::new(self.kind(), self)
    }

    /// Recover the typed error from an `io::Error` produced by [`AccessError::into_io`].
    #[must_use]
    pub fn from_io(error: &io::Error) -> Option<&AccessError> {
        error.get_ref().and_then(|inner| inner.downcast_ref())
    }
}

impl fmt::Display for AccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPublicKey(why) => write!(f, "{why}"),
            Self::KeyProofStale { skew_secs } => write!(
                f,
                "key registration proof timestamp is {skew_secs}s away from the server clock (limit {}s)",
                crate::signing::KEY_REGISTRATION_WINDOW_SECS
            ),
            Self::KeyProofInvalid(why) => {
                write!(f, "key registration proof of possession failed: {why}")
            }
            Self::ReservedThoughtType => f.write_str(
                "the AccessPolicy thought type is reserved for access-policy records",
            ),
            Self::SignatureRequired => {
                f.write_str("this chain requires every thought to carry a valid signature")
            }
            Self::MissingSigningKeyId => {
                f.write_str("a thought signature requires `signing_key_id`")
            }
            Self::UnsupportedSignature(why) => write!(f, "{why}"),
            Self::SignatureVersionNotAllowed(version) => write!(
                f,
                "{version:?} signatures are not accepted by this chain's access policy; sign with the v2 payload"
            ),
            Self::UnknownAgent(agent) => write!(f, "agent '{agent}' is not registered"),
            Self::UnknownKey { agent_id, key_id } => {
                write!(f, "agent '{agent_id}' has no key '{key_id}'")
            }
            Self::KeyRevoked { agent_id, key_id } => {
                write!(f, "key '{key_id}' of agent '{agent_id}' has been revoked")
            }
            Self::BadSignature(why) => write!(
                f,
                "{why} (a v2 signature also fails when the chain head moved; fetch the head and sign again)"
            ),
            Self::KeyNotAllowed { agent_id, key_id } => write!(
                f,
                "key '{key_id}' of agent '{agent_id}' is not in this chain's allowed key set"
            ),
            Self::PolicyInvalid(why) => write!(f, "invalid access policy record: {why}"),
            Self::NotAuthorized(why) => write!(f, "not authorized to change the access policy: {why}"),
        }
    }
}

impl std::error::Error for AccessError {}

/// Proof that the registrant holds the private key of a key being registered.
///
/// The signature covers [`crate::signing::key_registration_payload`] for the
/// chain, agent, key id, public key and `registered_at_unix_secs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRegistrationProof {
    /// Unix seconds the proof was made at; must be within
    /// [`crate::signing::KEY_REGISTRATION_WINDOW_SECS`] of the verifier's clock.
    pub registered_at_unix_secs: i64,
    /// Raw 64-byte Ed25519 signature made with the key being registered.
    pub signature: Vec<u8>,
}

impl KeyRegistrationProof {
    /// Check the proof for `public_key` against the verifier clock `now_unix_secs`.
    ///
    /// # Errors
    ///
    /// [`AccessError::InvalidPublicKey`], [`AccessError::KeyProofStale`] or
    /// [`AccessError::KeyProofInvalid`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ed25519_dalek::{Signer, SigningKey};
    /// use mentisdb::access::KeyRegistrationProof;
    /// use mentisdb::signing::key_registration_payload;
    ///
    /// let secret = SigningKey::from_bytes(&[8u8; 32]);
    /// let public = secret.verifying_key().to_bytes();
    /// let now = 1_700_000_000;
    /// let payload = key_registration_payload("main", "alice", "k1", &public, now);
    /// let proof = KeyRegistrationProof {
    ///     registered_at_unix_secs: now,
    ///     signature: secret.sign(&payload).to_bytes().to_vec(),
    /// };
    /// assert!(proof.verify("main", "alice", "k1", &public, now + 5).is_ok());
    /// assert!(proof.verify("main", "alice", "k1", &public, now + 5_000).is_err());
    /// assert!(proof.verify("other", "alice", "k1", &public, now).is_err());
    /// ```
    pub fn verify(
        &self,
        chain_key: &str,
        agent_id: &str,
        key_id: &str,
        public_key: &[u8],
        now_unix_secs: i64,
    ) -> Result<(), AccessError> {
        let key = crate::signing::validate_public_key(public_key)
            .map_err(AccessError::InvalidPublicKey)?;
        let skew = now_unix_secs.saturating_sub(self.registered_at_unix_secs);
        if skew.abs() > crate::signing::KEY_REGISTRATION_WINDOW_SECS {
            return Err(AccessError::KeyProofStale { skew_secs: skew });
        }
        let payload = crate::signing::key_registration_payload(
            chain_key,
            agent_id,
            key_id,
            &key,
            self.registered_at_unix_secs,
        );
        crate::signing::verify_ed25519_signature(&key, &payload, &self.signature)
            .map_err(AccessError::KeyProofInvalid)
    }
}

/// Resolve a byte string given either as a JSON array of integers or as hex.
///
/// REST and MCP clients may send `public_key_bytes: [..]` (the original form) or
/// `public_key_hex: "ab12.."`; exactly one must be present.
///
/// # Errors
///
/// A message naming `field` when neither or both forms are given, or the hex is
/// malformed.
///
/// # Examples
///
/// ```
/// use mentisdb::access::bytes_from_array_or_hex;
///
/// assert_eq!(bytes_from_array_or_hex(Some(vec![1, 2]), None, "key").unwrap(), vec![1, 2]);
/// assert_eq!(bytes_from_array_or_hex(None, Some("0102"), "key").unwrap(), vec![1, 2]);
/// assert!(bytes_from_array_or_hex(None, None, "key").is_err());
/// assert!(bytes_from_array_or_hex(Some(vec![1]), Some("01"), "key").is_err());
/// ```
pub fn bytes_from_array_or_hex(
    array: Option<Vec<u8>>,
    hex: Option<&str>,
    field: &str,
) -> Result<Vec<u8>, String> {
    match (array, hex) {
        (Some(bytes), None) => Ok(bytes),
        (None, Some(text)) => crate::signing::from_hex(text).map_err(|e| format!("{field}: {e}")),
        (None, None) => Err(format!(
            "{field} is required (as an array of bytes or as hex)"
        )),
        (Some(_), Some(_)) => Err(format!("give {field} as an array or as hex, not both")),
    }
}

// ── policy model ─────────────────────────────────────────────────────────────

/// `format` value of every policy record.
pub const POLICY_FORMAT: &str = "mentisdb/access-policy/v1";
/// Largest key set a policy record may carry.
pub const MAX_POLICY_KEYS: usize = 1024;

/// Who may append to a chain.
///
/// # Examples
///
/// ```
/// use mentisdb::access::AccessMode;
///
/// assert_eq!(AccessMode::default(), AccessMode::Open);
/// assert_eq!("allowlist".parse::<AccessMode>().unwrap(), AccessMode::Allowlist);
/// assert_eq!(AccessMode::Signed.to_string(), "signed");
/// assert!("closed".parse::<AccessMode>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// Anyone who can reach the API may append (the default).
    #[default]
    Open,
    /// Every thought must carry a valid v2 signature by an active key of its agent.
    Signed,
    /// Like `Signed`, and the key must be in the policy's allowed key set.
    Allowlist,
}

impl AccessMode {
    /// All modes, in strictness order.
    pub const ALL: [AccessMode; 3] = [Self::Open, Self::Signed, Self::Allowlist];

    /// Lowercase name used in records, APIs and the CLI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Signed => "signed",
            Self::Allowlist => "allowlist",
        }
    }
}

impl fmt::Display for AccessMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for an unknown access mode name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseAccessModeError(pub String);

impl fmt::Display for ParseAccessModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown access mode '{}'; expected open, signed or allowlist",
            self.0
        )
    }
}

impl std::error::Error for ParseAccessModeError {}

impl FromStr for AccessMode {
    type Err = ParseAccessModeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str().eq_ignore_ascii_case(input.trim()))
            .ok_or_else(|| ParseAccessModeError(input.to_string()))
    }
}

/// One entry of a policy's key set: an agent identity and its public key.
///
/// # Examples
///
/// ```
/// use mentisdb::access::AllowedKey;
///
/// let key = AllowedKey::new("alice", "alice-1", &[9u8; 32]);
/// assert_eq!(key.public_key, "09".repeat(32));
/// assert!(key.matches("alice", "alice-1", &[9u8; 32]));
/// assert!(!key.matches("alice", "alice-2", &[9u8; 32]));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedKey {
    /// Agent the key belongs to.
    pub agent_id: String,
    /// Key id within that agent.
    pub key_id: String,
    /// Public key as 64 lowercase hex characters.
    pub public_key: String,
}

impl AllowedKey {
    /// Build an entry from raw public-key bytes.
    #[must_use]
    pub fn new(agent_id: &str, key_id: &str, public_key: &[u8]) -> Self {
        Self {
            agent_id: agent_id.trim().to_string(),
            key_id: key_id.trim().to_string(),
            public_key: to_hex(public_key),
        }
    }

    /// Decode and validate the public key.
    ///
    /// # Errors
    ///
    /// [`AccessError::PolicyInvalid`] when the hex is malformed or the key is not
    /// a strong curve point.
    pub fn public_key_bytes(&self) -> Result<[u8; 32], AccessError> {
        from_hex(&self.public_key)
            .map_err(|e| AccessError::PolicyInvalid(format!("key '{}': {e}", self.key_id)))
            .and_then(|bytes| {
                crate::signing::validate_public_key(&bytes)
                    .map_err(|e| AccessError::PolicyInvalid(format!("key '{}': {e}", self.key_id)))
            })
    }

    /// The Merkle leaf of this entry.
    ///
    /// # Errors
    ///
    /// As [`AllowedKey::public_key_bytes`].
    pub fn leaf(&self) -> Result<[u8; 32], AccessError> {
        Ok(access_key_leaf(
            &self.agent_id,
            &self.key_id,
            &self.public_key_bytes()?,
        ))
    }

    /// Whether this entry names exactly this agent, key id and public key.
    #[must_use]
    pub fn matches(&self, agent_id: &str, key_id: &str, public_key: &[u8]) -> bool {
        self.agent_id == agent_id && self.key_id == key_id && self.public_key == to_hex(public_key)
    }
}

/// The JSON object stored as the `content` of an `AccessPolicy` thought.
///
/// Field order is the canonical order; [`PolicyRecord::to_content`] and
/// [`PolicyRecord::parse`] only accept that exact encoding.
///
/// # Examples
///
/// ```
/// use mentisdb::access::{AccessMode, AllowedKey, PolicyRecord};
///
/// let record = PolicyRecord::new(
///     0,
///     AccessMode::Allowlist,
///     vec![AllowedKey::new("alice", "k1", &[0u8; 32])],
///     "",
/// );
/// // A zero key is not a valid point, so construction fails.
/// assert!(record.is_err());
///
/// use ed25519_dalek::SigningKey;
/// let public = SigningKey::from_bytes(&[1u8; 32]).verifying_key().to_bytes();
/// let record = PolicyRecord::new(0, AccessMode::Allowlist, vec![AllowedKey::new("alice", "k1", &public)], "").unwrap();
/// let content = record.to_content();
/// assert_eq!(PolicyRecord::parse(&content).unwrap(), record);
/// assert_eq!(record.keys_root.len(), 64);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRecord {
    /// Always [`POLICY_FORMAT`].
    pub format: String,
    /// 0 for the first record, then previous + 1.
    pub seq: u64,
    /// The mode in force after this record.
    pub mode: AccessMode,
    /// Governor / allow set, sorted by Merkle leaf.
    pub keys: Vec<AllowedKey>,
    /// Hex Merkle root committing to `keys`.
    pub keys_root: String,
    /// Hash of the previous policy thought, empty for `seq` 0.
    pub prev_policy: String,
}

impl PolicyRecord {
    /// Build a record: validates and sorts `keys` and computes the root.
    ///
    /// # Errors
    ///
    /// [`AccessError::PolicyInvalid`] for an invalid or duplicate key, too many
    /// keys, or an empty set with a mode that needs keys.
    pub fn new(
        seq: u64,
        mode: AccessMode,
        keys: Vec<AllowedKey>,
        prev_policy: &str,
    ) -> Result<Self, AccessError> {
        let mut leaves = keys
            .into_iter()
            .map(|key| key.leaf().map(|leaf| (leaf, key)))
            .collect::<Result<Vec<_>, _>>()?;
        leaves.sort_by_key(|(leaf, _)| *leaf);
        let sorted: Vec<[u8; 32]> = leaves.iter().map(|(leaf, _)| *leaf).collect();
        let record = Self {
            format: POLICY_FORMAT.to_string(),
            seq,
            mode,
            keys: leaves.into_iter().map(|(_, key)| key).collect(),
            keys_root: to_hex(&merkle_root(&sorted)),
            prev_policy: prev_policy.to_string(),
        };
        record.validate()?;
        Ok(record)
    }

    /// The canonical JSON content of the governance thought.
    #[must_use]
    pub fn to_content(&self) -> String {
        serde_json::to_string(self).expect("policy record serializes")
    }

    /// Parse canonical content.
    ///
    /// # Errors
    ///
    /// [`AccessError::PolicyInvalid`] when the JSON is malformed, has unknown or
    /// missing fields, is not byte-for-byte the canonical encoding, or fails
    /// [`PolicyRecord::validate`].
    pub fn parse(content: &str) -> Result<Self, AccessError> {
        let record: Self = serde_json::from_str(content)
            .map_err(|e| AccessError::PolicyInvalid(format!("malformed record: {e}")))?;
        if record.to_content() != content {
            return Err(AccessError::PolicyInvalid(
                "record is not in canonical form".to_string(),
            ));
        }
        record.validate()?;
        Ok(record)
    }

    /// Check every rule that does not depend on the chain's history.
    ///
    /// # Errors
    ///
    /// [`AccessError::PolicyInvalid`] naming the first rule that fails.
    pub fn validate(&self) -> Result<(), AccessError> {
        let bad = |why: &str| Err(AccessError::PolicyInvalid(why.to_string()));
        if self.format != POLICY_FORMAT {
            return bad("unknown format");
        }
        if self.keys.len() > MAX_POLICY_KEYS {
            return bad("too many keys");
        }
        if self.mode != AccessMode::Open && self.keys.is_empty() {
            return bad("signed and allowlist modes need at least one key");
        }
        if self
            .keys
            .iter()
            .any(|k| k.agent_id.trim().is_empty() || k.key_id.trim().is_empty())
        {
            return bad("agent_id and key_id must not be empty");
        }
        let leaves = self
            .keys
            .iter()
            .map(AllowedKey::leaf)
            .collect::<Result<Vec<_>, _>>()?;
        if leaves.windows(2).any(|w| w[0] >= w[1]) {
            return bad("keys must be sorted by leaf hash without duplicates");
        }
        let mut identities: Vec<(&str, &str)> = self
            .keys
            .iter()
            .map(|k| (k.agent_id.as_str(), k.key_id.as_str()))
            .collect();
        identities.sort_unstable();
        if identities.windows(2).any(|w| w[0] == w[1]) {
            return bad("duplicate (agent_id, key_id)");
        }
        if self.keys_root != to_hex(&merkle_root(&leaves)) {
            return bad("keys_root does not match keys");
        }
        Ok(())
    }
}

/// The access policy in force on a chain, derived by replaying its governance records.
///
/// Chains without records are [`AccessMode::Open`].
///
/// # Examples
///
/// ```
/// use mentisdb::access::{AccessMode, AccessPolicy};
///
/// let policy = AccessPolicy::default();
/// assert_eq!(policy.mode, AccessMode::Open);
/// assert!(policy.seq.is_none() && policy.keys.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct AccessPolicy {
    /// Current mode.
    pub mode: AccessMode,
    /// `seq` of the latest valid record, if any.
    pub seq: Option<u64>,
    /// Current governor / allow set.
    pub keys: Vec<AllowedKey>,
    /// Hex Merkle root of `keys` (of the empty set when there is no record).
    pub keys_root: String,
    /// Hash of the genesis (`seq` 0) record thought.
    pub genesis_hash: Option<String>,
    /// Hash of the latest valid record thought.
    pub head_record_hash: Option<String>,
    /// Hashes of policy thoughts found in the chain that failed validation and are ignored.
    pub invalid_records: Vec<String>,
}

impl AccessPolicy {
    /// Whether `(agent_id, key_id, public_key)` is in the current key set.
    #[must_use]
    pub fn contains(&self, agent_id: &str, key_id: &str, public_key: &[u8]) -> bool {
        self.keys
            .iter()
            .any(|k| k.matches(agent_id, key_id, public_key))
    }

    /// Check a proposed record against the current state and the signer's authority.
    ///
    /// `payload` is the v2 signing payload of the governance thought and
    /// `signature` its stored `thought_signature`; the signer is
    /// `(agent_id, key_id)`.
    ///
    /// # Errors
    ///
    /// [`AccessError::PolicyInvalid`] for a bad sequence or link,
    /// [`AccessError::NotAuthorized`] when the signer is not a governor (for
    /// `seq` 0: not in the record's own key set), or a signature error.
    pub fn check_record(
        &self,
        record: &PolicyRecord,
        agent_id: &str,
        key_id: &str,
        signature: &[u8],
        payload: &[u8],
    ) -> Result<(), AccessError> {
        record.validate()?;
        let expected_seq = self.seq.map_or(0, |seq| seq + 1);
        if record.seq != expected_seq {
            return Err(AccessError::PolicyInvalid(format!(
                "expected seq {expected_seq}, got {}",
                record.seq
            )));
        }
        let expected_prev = self.head_record_hash.as_deref().unwrap_or("");
        if record.prev_policy != expected_prev {
            return Err(AccessError::PolicyInvalid(
                "prev_policy does not match the previous record".to_string(),
            ));
        }
        let stored =
            StoredSignature::parse(signature).map_err(AccessError::UnsupportedSignature)?;
        if stored.version != SignatureVersion::V2 {
            return Err(AccessError::SignatureVersionNotAllowed(stored.version));
        }
        let governors = if record.seq == 0 {
            &record.keys
        } else {
            &self.keys
        };
        let signer = governors
            .iter()
            .find(|k| k.agent_id == agent_id && k.key_id == key_id)
            .ok_or_else(|| {
                AccessError::NotAuthorized(format!(
                    "key '{key_id}' of agent '{agent_id}' is not a governor"
                ))
            })?;
        verify_ed25519_signature(&signer.public_key_bytes()?, payload, &stored.signature)
            .map_err(AccessError::BadSignature)
    }

    /// Replace the state with `record`, committed as the thought with `hash`.
    pub fn commit_record(&mut self, record: PolicyRecord, hash: &str) {
        if record.seq == 0 {
            self.genesis_hash = Some(hash.to_string());
        }
        self.mode = record.mode;
        self.seq = Some(record.seq);
        self.keys = record.keys;
        self.keys_root = record.keys_root;
        self.head_record_hash = Some(hash.to_string());
    }

    /// Validate a stored governance thought against the current state and, if
    /// valid, apply it.
    ///
    /// # Errors
    ///
    /// As [`AccessPolicy::check_record`]; the state is unchanged on error.
    pub fn apply_thought(&mut self, chain_key: &str, thought: &Thought) -> Result<(), AccessError> {
        let record = PolicyRecord::parse(&thought.content)?;
        let signature = thought
            .thought_signature
            .as_deref()
            .ok_or(AccessError::SignatureRequired)?;
        let key_id = thought
            .signing_key_id
            .as_deref()
            .ok_or(AccessError::MissingSigningKeyId)?;
        let payload = thought_payload_v2_stored(chain_key, thought);
        self.check_record(&record, &thought.agent_id, key_id, signature, &payload)?;
        self.commit_record(record, &thought.hash);
        Ok(())
    }

    /// Rebuild the policy by replaying every `AccessPolicy` thought in order.
    /// Records that fail validation are skipped and listed in `invalid_records`.
    #[must_use]
    pub fn replay(chain_key: &str, thoughts: &[Thought]) -> Self {
        let mut policy = Self {
            keys_root: to_hex(&merkle_root(&[])),
            ..Self::default()
        };
        for thought in thoughts
            .iter()
            .filter(|t| t.thought_type == ThoughtType::AccessPolicy)
        {
            if policy.apply_thought(chain_key, thought).is_err() {
                policy.invalid_records.push(thought.hash.clone());
            }
        }
        policy
    }
}

/// The unsigned thought input a governance record is committed as.
///
/// Signers build their v2 payload from this input
/// ([`crate::signing::thought_payload_v2`]) and then set the signature fields;
/// [`crate::MentisDb::set_access_policy`] commits exactly this shape: type
/// `AccessPolicy`, role `Audit`, importance 1.0, no tags, concepts, refs or
/// relations.
///
/// # Examples
///
/// ```
/// use mentisdb::access::{policy_thought_input, AccessMode, PolicyRecord};
/// use mentisdb::ThoughtType;
///
/// let record = PolicyRecord::new(0, AccessMode::Open, vec![], "").unwrap();
/// let input = policy_thought_input(&record);
/// assert_eq!(input.thought_type, ThoughtType::AccessPolicy);
/// assert_eq!(input.content, record.to_content());
/// ```
#[must_use]
pub fn policy_thought_input(record: &PolicyRecord) -> ThoughtInput {
    let mut input = ThoughtInput::new(ThoughtType::AccessPolicy, record.to_content())
        .with_role(ThoughtRole::Audit);
    input.importance = 1.0;
    input
}

// ── signature report ─────────────────────────────────────────────────────────

/// Outcome of re-verifying one stored thought.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum SignatureStatus {
    /// The signature verifies.
    Valid,
    /// The thought carries no signature.
    Unsigned,
    /// The signature is present but does not verify, or the key is unknown.
    Invalid(String),
}

/// Re-verification result for one thought.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignatureCheck {
    /// Position in the chain.
    pub index: u64,
    /// Thought id.
    pub thought_id: Uuid,
    /// Claimed author.
    pub agent_id: String,
    /// Claimed signing key id.
    pub key_id: Option<String>,
    /// Payload version recorded on the thought (`1` or `2`).
    pub version: Option<u8>,
    /// Verification outcome.
    #[serde(flatten)]
    pub status: SignatureStatus,
}

/// Result of [`crate::MentisDb::verify_signatures`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignatureReport {
    /// Chain that was checked.
    pub chain_key: String,
    /// Policy in force at the head.
    pub policy: AccessPolicy,
    /// One entry per thought, in chain order.
    pub checks: Vec<SignatureCheck>,
    /// Thoughts carrying a signature.
    pub signed: usize,
    /// Signed thoughts that verify.
    pub valid: usize,
    /// Thoughts without a signature.
    pub unsigned: usize,
    /// Signed thoughts that do not verify.
    pub invalid: usize,
}

impl SignatureReport {
    /// True when no signed thought failed and no policy record was rejected.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.invalid == 0 && self.policy.invalid_records.is_empty()
    }
}
