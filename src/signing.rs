//! Ed25519 signing primitives for MentisDB: key validation, signature
//! verification, domain-separated canonical payloads and the key-set Merkle
//! commitment.
//!
//! Everything that signs or verifies in MentisDB goes through this module:
//! skill upload, thought append, key registration and access-policy records.
//! The byte formats are specified in `docs/CHAIN_ACCESS.md`; this module is the
//! reference implementation and other programs (for example `mentischat`) can
//! reproduce them from that document.
//!
//! # Formats at a glance
//!
//! * Public key: 32 raw bytes, text form lowercase hex ([`to_hex`], [`from_hex`]).
//! * Signature: 64 raw bytes (pure Ed25519, RFC 8032).
//! * Every signed preimage starts with an ASCII domain tag ending in NUL
//!   ([`THOUGHT_V2_DOMAIN`], [`KEY_REGISTRATION_DOMAIN`]).
//! * A stored thought's `thought_signature` is either 64 bytes (v1, legacy) or
//!   65 bytes (`0x02` then the 64-byte v2 signature); see [`StoredSignature`].
//!
//! # Examples
//!
//! ```
//! use ed25519_dalek::{Signer, SigningKey};
//! use mentisdb::signing::{validate_public_key, verify_ed25519_signature};
//!
//! let secret = SigningKey::from_bytes(&[7u8; 32]);
//! let public = secret.verifying_key().to_bytes();
//! assert!(validate_public_key(&public).is_ok());
//!
//! let signature = secret.sign(b"message").to_bytes();
//! assert!(verify_ed25519_signature(&public, b"message", &signature).is_ok());
//! assert!(verify_ed25519_signature(&public, b"tampered", &signature).is_err());
//! ```

use crate::{normalize_strings, Thought, ThoughtInput, ThoughtRelation};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt;
use uuid::Uuid;

/// Domain tag of the thought v2 payload.
pub const THOUGHT_V2_DOMAIN: &[u8] = b"mentisdb/thought/v2\0";
/// Domain tag of the key-registration proof-of-possession payload.
pub const KEY_REGISTRATION_DOMAIN: &[u8] = b"mentisdb/key-registration/v1\0";
/// Domain tag of an access-key Merkle leaf.
pub const ACCESS_KEY_LEAF_DOMAIN: &[u8] = b"mentisdb/access-key/v1\0";
/// Length of an Ed25519 public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Length of an Ed25519 signature in bytes.
pub const SIGNATURE_LEN: usize = 64;
/// Leading byte of a v2 [`StoredSignature`].
pub const SIGNATURE_V2_TAG: u8 = 0x02;
/// How far (seconds) a key-registration timestamp may differ from the server clock.
pub const KEY_REGISTRATION_WINDOW_SECS: i64 = 600;

/// Why a key, signature or hex string was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningError {
    /// The public key is not 32 bytes.
    PublicKeyLength(usize),
    /// The public key does not decode to a curve point.
    PublicKeyNotOnCurve,
    /// The public key is a small-order (weak) point.
    PublicKeyWeak,
    /// The signature is not 64 bytes.
    SignatureLength(usize),
    /// The stored signature has an unknown length or leading version byte.
    UnsupportedSignatureVersion,
    /// The signature does not verify for the message and key.
    VerificationFailed,
    /// A hex string had an odd length, a non-hex digit, or the wrong size.
    InvalidHex(String),
}

impl fmt::Display for SigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PublicKeyLength(n) => write!(
                f,
                "invalid Ed25519 public key length: expected 32 bytes, got {n}"
            ),
            Self::PublicKeyNotOnCurve => f.write_str("invalid Ed25519 public key: not a curve point"),
            Self::PublicKeyWeak => f.write_str("invalid Ed25519 public key: small-order point"),
            Self::SignatureLength(n) => write!(
                f,
                "invalid Ed25519 signature length: expected 64 bytes, got {n}"
            ),
            Self::UnsupportedSignatureVersion => f.write_str(
                "unsupported thought signature: expected 64 bytes (v1) or 0x02 followed by 64 bytes (v2)",
            ),
            Self::VerificationFailed => f.write_str("Ed25519 signature verification failed"),
            Self::InvalidHex(why) => write!(f, "invalid hex: {why}"),
        }
    }
}

impl std::error::Error for SigningError {}

// ── hex ──────────────────────────────────────────────────────────────────────

/// Encode bytes as lowercase hex.
///
/// # Examples
///
/// ```
/// assert_eq!(mentisdb::signing::to_hex(&[0, 15, 255]), "000fff");
/// ```
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]])
        .map(char::from)
        .collect()
}

/// Decode hex (either case, surrounding whitespace ignored) into bytes.
///
/// # Errors
///
/// [`SigningError::InvalidHex`] for an empty string, an odd number of digits or a
/// character that is not a hex digit.
///
/// # Examples
///
/// ```
/// use mentisdb::signing::from_hex;
///
/// assert_eq!(from_hex("000FfF").unwrap(), vec![0, 15, 255]);
/// assert!(from_hex("abc").is_err());
/// assert!(from_hex("zz").is_err());
/// ```
pub fn from_hex(text: &str) -> Result<Vec<u8>, SigningError> {
    let text = text.trim();
    let digit = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(SigningError::InvalidHex(format!(
            "'{}' is not a hex digit",
            char::from(c)
        ))),
    };
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return Err(SigningError::InvalidHex(
            "expected a non-empty even number of digits".to_string(),
        ));
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

// ── keys and signatures ──────────────────────────────────────────────────────

/// Check that `bytes` is a usable Ed25519 public key and return it as an array.
///
/// A usable key is exactly 32 bytes, decompresses to a curve point and is not a
/// small-order (weak) point.
///
/// # Errors
///
/// [`SigningError::PublicKeyLength`], [`SigningError::PublicKeyNotOnCurve`] or
/// [`SigningError::PublicKeyWeak`].
///
/// # Examples
///
/// ```
/// use mentisdb::signing::{validate_public_key, SigningError};
///
/// assert_eq!(validate_public_key(&[1u8; 5]), Err(SigningError::PublicKeyLength(5)));
/// // The identity point (y = 1) is a valid curve point of order 1: weak.
/// let mut identity = [0u8; 32];
/// identity[0] = 1;
/// assert_eq!(validate_public_key(&identity), Err(SigningError::PublicKeyWeak));
/// ```
pub fn validate_public_key(bytes: &[u8]) -> Result<[u8; PUBLIC_KEY_LEN], SigningError> {
    parse_public_key(bytes).map(|key| key.to_bytes())
}

/// Decode and validate a public key once, for callers that go on to verify.
fn parse_public_key(bytes: &[u8]) -> Result<VerifyingKey, SigningError> {
    let array: [u8; PUBLIC_KEY_LEN] = bytes
        .try_into()
        .map_err(|_| SigningError::PublicKeyLength(bytes.len()))?;
    let key = VerifyingKey::from_bytes(&array).map_err(|_| SigningError::PublicKeyNotOnCurve)?;
    if key.is_weak() {
        return Err(SigningError::PublicKeyWeak);
    }
    Ok(key)
}

/// Verify a detached Ed25519 `signature` over `message` with `public_key_bytes`.
///
/// This is the single verification routine for MentisDB: skill upload, thought
/// append, key registration and policy records all call it. It uses strict
/// verification, so weak public keys and non-canonical signatures are refused.
///
/// # Errors
///
/// A [`SigningError`] describing a bad key, a bad signature length, or a
/// signature that does not verify.
///
/// # Examples
///
/// ```
/// use ed25519_dalek::{Signer, SigningKey};
/// use mentisdb::signing::verify_ed25519_signature;
///
/// let secret = SigningKey::from_bytes(&[1u8; 32]);
/// let public = secret.verifying_key().to_bytes();
/// let signature = secret.sign(b"hello").to_bytes();
///
/// assert!(verify_ed25519_signature(&public, b"hello", &signature).is_ok());
/// assert!(verify_ed25519_signature(&public, b"hello", &signature[..63]).is_err());
/// ```
pub fn verify_ed25519_signature(
    public_key_bytes: &[u8],
    message: &[u8],
    signature_bytes: &[u8],
) -> Result<(), SigningError> {
    let key = parse_public_key(public_key_bytes)?;
    let sig: [u8; SIGNATURE_LEN] = signature_bytes
        .try_into()
        .map_err(|_| SigningError::SignatureLength(signature_bytes.len()))?;
    key.verify_strict(message, &Signature::from_bytes(&sig))
        .map_err(|_| SigningError::VerificationFailed)
}

/// The version of the payload a stored thought signature covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureVersion {
    /// Legacy payload ([`crate::signable_thought_payload`]); not bound to a chain or position.
    V1,
    /// Domain-separated payload bound to chain and head ([`thought_payload_v2`]).
    V2,
}

/// A parsed `Thought::thought_signature` value.
///
/// # Examples
///
/// ```
/// use mentisdb::signing::{SignatureVersion, StoredSignature};
///
/// let v1 = StoredSignature::parse(&[0u8; 64]).unwrap();
/// assert_eq!(v1.version, SignatureVersion::V1);
///
/// let mut raw = vec![0x02];
/// raw.extend([0u8; 64]);
/// let v2 = StoredSignature::parse(&raw).unwrap();
/// assert_eq!(v2.version, SignatureVersion::V2);
/// assert_eq!(v2.signature, [0u8; 64]);
/// assert!(StoredSignature::parse(&[0u8; 10]).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredSignature {
    /// Which payload the signature covers.
    pub version: SignatureVersion,
    /// The raw 64-byte signature.
    pub signature: [u8; SIGNATURE_LEN],
}

impl StoredSignature {
    /// Parse the stored bytes: 64 bytes is v1, `0x02` + 64 bytes is v2.
    ///
    /// # Errors
    ///
    /// [`SigningError::UnsupportedSignatureVersion`] for any other shape.
    pub fn parse(raw: &[u8]) -> Result<Self, SigningError> {
        match raw.len() {
            SIGNATURE_LEN => Ok(Self {
                version: SignatureVersion::V1,
                signature: raw.try_into().expect("length checked"),
            }),
            n if n == SIGNATURE_LEN + 1 && raw[0] == SIGNATURE_V2_TAG => Ok(Self {
                version: SignatureVersion::V2,
                signature: raw[1..].try_into().expect("length checked"),
            }),
            _ => Err(SigningError::UnsupportedSignatureVersion),
        }
    }

    /// Encode a raw v2 signature the way it is stored on a thought.
    #[must_use]
    pub fn encode_v2(signature: &[u8; SIGNATURE_LEN]) -> Vec<u8> {
        let mut out = Vec::with_capacity(SIGNATURE_LEN + 1);
        out.push(SIGNATURE_V2_TAG);
        out.extend_from_slice(signature);
        out
    }
}

// ── canonical payload encoding ───────────────────────────────────────────────

/// Writer for the primitive encodings in `docs/CHAIN_ACCESS.md`.
struct Payload(Vec<u8>);

impl Payload {
    fn new(domain: &[u8]) -> Self {
        Self(domain.to_vec())
    }
    fn str(&mut self, value: &str) -> &mut Self {
        self.0.extend((value.len() as u32).to_be_bytes());
        self.0.extend(value.as_bytes());
        self
    }
    fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.0.extend(bytes);
        self
    }
    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }
    fn u64(&mut self, value: u64) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }
    fn i64(&mut self, value: i64) -> &mut Self {
        self.0.extend(value.to_be_bytes());
        self
    }
    fn f32(&mut self, value: f32) -> &mut Self {
        self.0.extend(value.clamp(0.0, 1.0).to_bits().to_be_bytes());
        self
    }
    fn opt<T>(&mut self, value: Option<T>, write: impl FnOnce(&mut Self, T)) -> &mut Self {
        match value {
            None => self.0.push(0),
            Some(inner) => {
                self.0.push(1);
                write(self, inner);
            }
        }
        self
    }
    fn strings(&mut self, values: &[String]) -> &mut Self {
        self.u32(values.len() as u32);
        values.iter().for_each(|v| {
            self.str(v);
        });
        self
    }
    fn time(&mut self, value: &DateTime<Utc>) -> &mut Self {
        self.i64(value.timestamp())
            .u32(value.timestamp_subsec_nanos())
    }
    fn relations(&mut self, relations: &[ThoughtRelation]) -> &mut Self {
        self.u32(relations.len() as u32);
        for relation in relations {
            self.str(&format!("{:?}", relation.kind));
            self.raw(relation.target_id.as_bytes());
            self.opt(relation.chain_key.as_deref(), |p, v| {
                p.str(v);
            });
            self.opt(relation.valid_at.as_ref(), |p, v| {
                p.time(v);
            });
            self.opt(relation.invalid_at.as_ref(), |p, v| {
                p.time(v);
            });
        }
        self
    }
}

/// Borrowed view of the semantic fields a v2 signature covers.
struct ThoughtFields<'a> {
    session_id: Option<Uuid>,
    thought_type: &'a str,
    role: String,
    content: &'a str,
    confidence: Option<f32>,
    importance: f32,
    tags: &'a [String],
    concepts: &'a [String],
    refs: &'a [u64],
    relations: &'a [ThoughtRelation],
    entity_type: Option<&'a str>,
    source_episode: Option<&'a str>,
}

fn encode_thought_v2(
    chain_key: &str,
    agent_id: &str,
    prev_hash: &str,
    fields: &ThoughtFields<'_>,
) -> Vec<u8> {
    let mut p = Payload::new(THOUGHT_V2_DOMAIN);
    p.str(chain_key).str(agent_id).str(prev_hash);
    p.opt(fields.session_id, |p, v| {
        p.raw(v.as_bytes());
    });
    p.str(fields.thought_type)
        .str(&fields.role)
        .str(fields.content);
    p.opt(fields.confidence, |p, v| {
        p.f32(v);
    });
    p.f32(fields.importance);
    p.strings(fields.tags).strings(fields.concepts);
    p.u32(fields.refs.len() as u32);
    fields.refs.iter().for_each(|r| {
        p.u64(*r);
    });
    p.relations(fields.relations);
    p.opt(fields.entity_type, |p, v| {
        p.str(v);
    });
    p.opt(fields.source_episode, |p, v| {
        p.str(v);
    });
    p.0
}

/// Build the v2 payload a signer signs for a thought that is about to be
/// appended to `chain_key` after the thought whose hash is `prev_hash`.
///
/// The payload is built from the same normalization the chain applies on append
/// (trimmed, de-duplicated tags and concepts; clamped scores), so signing and
/// verification agree. `prev_hash` is the chain head hash (`""` for an empty
/// chain), available from `chain head` / `mentisdb_head`.
///
/// # Examples
///
/// ```
/// use mentisdb::signing::{thought_payload_v2, THOUGHT_V2_DOMAIN};
/// use mentisdb::{ThoughtInput, ThoughtType};
///
/// let input = ThoughtInput::new(ThoughtType::Decision, "ship it");
/// let a = thought_payload_v2("main", "alice", "", &input);
/// assert!(a.starts_with(THOUGHT_V2_DOMAIN));
///
/// // Bound to the chain and to the position.
/// assert_ne!(a, thought_payload_v2("other", "alice", "", &input));
/// assert_ne!(a, thought_payload_v2("main", "alice", "abc", &input));
/// ```
#[must_use]
pub fn thought_payload_v2(
    chain_key: &str,
    agent_id: &str,
    prev_hash: &str,
    input: &ThoughtInput,
) -> Vec<u8> {
    let tags = normalize_strings(input.tags.clone());
    let concepts = normalize_strings(input.concepts.clone());
    let entity_type = input
        .entity_type
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    encode_thought_v2(
        chain_key,
        agent_id,
        prev_hash,
        &ThoughtFields {
            session_id: input.session_id,
            thought_type: input.thought_type.as_str(),
            role: format!("{:?}", input.role),
            content: &input.content,
            confidence: input.confidence,
            importance: input.importance,
            tags: &tags,
            concepts: &concepts,
            refs: &input.refs,
            relations: &input.relations,
            entity_type,
            source_episode: input.source_episode.as_deref(),
        },
    )
}

/// Rebuild the v2 payload of a stored thought, for re-verification.
///
/// Equal to [`thought_payload_v2`] of the input the thought was committed from.
///
/// # Examples
///
/// ```
/// use mentisdb::signing::thought_payload_v2_stored;
/// use mentisdb::{MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
///
/// let dir = std::env::temp_dir().join(format!("mentisdb_doc_payload_{}", std::process::id()));
/// let mut chain = MentisDb::open_with_key_and_storage_kind(&dir, "doc", StorageAdapterKind::Binary).unwrap();
/// let t = chain.append("alice", ThoughtType::Insight, "hello").unwrap().clone();
/// assert!(!thought_payload_v2_stored("doc", &t).is_empty());
/// # let _ = std::fs::remove_dir_all(&dir);
/// ```
#[must_use]
pub fn thought_payload_v2_stored(chain_key: &str, thought: &Thought) -> Vec<u8> {
    encode_thought_v2(
        chain_key,
        &thought.agent_id,
        &thought.prev_hash,
        &ThoughtFields {
            session_id: thought.session_id,
            thought_type: thought.thought_type.as_str(),
            role: format!("{:?}", thought.role),
            content: &thought.content,
            confidence: thought.confidence,
            importance: thought.importance,
            tags: &thought.tags,
            concepts: &thought.concepts,
            refs: &thought.refs,
            relations: &thought.relations,
            entity_type: thought.entity_type.as_deref(),
            source_episode: thought.source_episode.as_deref(),
        },
    )
}

/// Build the proof-of-possession payload for registering `public_key` as
/// `key_id` of `agent_id` on `chain_key`.
///
/// The key owner signs this with the key being registered;
/// [`crate::MentisDb::add_agent_key`] verifies it. `registered_at_unix_secs`
/// must be within [`KEY_REGISTRATION_WINDOW_SECS`] of the server clock.
///
/// # Examples
///
/// ```
/// use ed25519_dalek::{Signer, SigningKey};
/// use mentisdb::signing::{key_registration_payload, verify_ed25519_signature};
///
/// let secret = SigningKey::from_bytes(&[3u8; 32]);
/// let public = secret.verifying_key().to_bytes();
/// let payload = key_registration_payload("main", "alice", "alice-1", &public, 1_700_000_000);
/// let proof = secret.sign(&payload).to_bytes();
/// assert!(verify_ed25519_signature(&public, &payload, &proof).is_ok());
/// ```
#[must_use]
pub fn key_registration_payload(
    chain_key: &str,
    agent_id: &str,
    key_id: &str,
    public_key: &[u8],
    registered_at_unix_secs: i64,
) -> Vec<u8> {
    let mut p = Payload::new(KEY_REGISTRATION_DOMAIN);
    p.str(chain_key).str(agent_id).str(key_id);
    p.raw(public_key).i64(registered_at_unix_secs);
    p.0
}

// ── key-set Merkle commitment ────────────────────────────────────────────────

/// The Merkle leaf hash of one allowed key.
///
/// `SHA256(0x00 || "mentisdb/access-key/v1\0" || str agent_id || str key_id || public_key)`.
///
/// # Examples
///
/// ```
/// let leaf = mentisdb::signing::access_key_leaf("alice", "k1", &[1u8; 32]);
/// assert_eq!(leaf.len(), 32);
/// assert_ne!(leaf, mentisdb::signing::access_key_leaf("alice", "k2", &[1u8; 32]));
/// ```
#[must_use]
pub fn access_key_leaf(agent_id: &str, key_id: &str, public_key: &[u8]) -> [u8; 32] {
    let mut p = Payload(vec![0x00]);
    p.raw(ACCESS_KEY_LEAF_DOMAIN).str(agent_id).str(key_id);
    p.raw(public_key);
    Sha256::digest(&p.0).into()
}

/// RFC 6962 Merkle tree hash over already-sorted `leaves`.
///
/// An empty set hashes to `SHA256("")`; node = `SHA256(0x01 || left || right)`
/// with the split at the largest power of two below the leaf count.
///
/// # Examples
///
/// ```
/// use mentisdb::signing::merkle_root;
///
/// let a = [1u8; 32];
/// assert_eq!(merkle_root(&[a]), a);
/// assert_ne!(merkle_root(&[a, [2u8; 32]]), merkle_root(&[[2u8; 32], a]));
/// ```
#[must_use]
pub fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => Sha256::digest([]).into(),
        1 => leaves[0],
        n => {
            let split = 1usize << (usize::BITS - 1 - (n - 1).leading_zeros());
            let (left, right) = leaves.split_at(split);
            let mut hasher = Sha256::new();
            hasher.update([0x01]);
            hasher.update(merkle_root(left));
            hasher.update(merkle_root(right));
            hasher.finalize().into()
        }
    }
}
