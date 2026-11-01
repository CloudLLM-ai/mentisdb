//! Request-level proof of possession of an authorized key, used to read the
//! private segments of a chain.
//!
//! A client that holds an Ed25519 key listed in a chain's access policy signs
//! each HTTP request locally and sends the result in one header,
//! [`READ_AUTH_HEADER`]. The private key never travels over the wire and never
//! needs to enter an LLM's context: the process that talks to the server (the
//! `mentisdb` CLI, the stdio MCP proxy, an SDK) holds the key file and signs.
//!
//! ```text
//! X-Mentis-Auth: v1 agent=alice;key=alice-202610;pk=<64 hex>;ts=1767225600;nonce=<32 hex>;sig=<128 hex>
//! ```
//!
//! The signature covers (see `docs/CHAIN_ACCESS.md`, *Read authentication*):
//!
//! ```text
//! "mentisdb/read-auth/v1\0"
//! str METHOD  str path_and_query  bytes[32] sha256(body)
//! str agent_id  str key_id  bytes[32] public_key  i64 ts  str nonce
//! ```
//!
//! so it is bound to one method, one URL, one body (and therefore one chain
//! key, which travels in the body), one key and one moment. A request is
//! accepted when the timestamp is within [`READ_AUTH_WINDOW_SECS`] of the
//! server clock and its nonce has not been seen inside that window
//! ([`NonceCache`]), so a captured request cannot be replayed, and cannot be
//! turned into a different request.
//!
//! The proof grants nothing by itself. The server verifies it and then asks, per
//! chain, whether `(agent_id, key_id, public_key)` is in that chain's current
//! authorized key set; only then does the request see private segments.
//!
//! The proof authenticates the reader, it does not encrypt the response. Serve
//! private chains over TLS.
//!
//! # Examples
//!
//! ```
//! use mentisdb::read_auth::{sign_request, verify_request};
//!
//! let seed = [7u8; 32];
//! let body = br#"{"chain_key":"vault","text":"launch"}"#;
//! let now = 1_767_225_600;
//! let header = sign_request(&seed, "alice", "alice-1", "POST", "/v1/ranked-search", body, now, "00112233445566778899aabbccddeeff");
//!
//! let proof = verify_request(&header, "POST", "/v1/ranked-search", body, now + 5).unwrap();
//! assert_eq!(proof.agent_id, "alice");
//!
//! // A different body, path or method does not verify.
//! assert!(verify_request(&header, "POST", "/v1/ranked-search", b"{}", now).is_err());
//! assert!(verify_request(&header, "POST", "/v1/search", body, now).is_err());
//! // Neither does the same request an hour later.
//! assert!(verify_request(&header, "POST", "/v1/ranked-search", body, now + 3600).is_err());
//! ```

use crate::signing::{from_hex, to_hex, verify_ed25519_signature, Payload};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// Lowercase name of the HTTP header that carries the proof.
pub const READ_AUTH_HEADER: &str = "x-mentis-auth";
/// Domain tag every read-auth preimage starts with.
pub const READ_AUTH_DOMAIN: &[u8] = b"mentisdb/read-auth/v1\0";
/// Seconds a proof stays valid on either side of the server clock.
pub const READ_AUTH_WINDOW_SECS: i64 = 60;
/// Longest header value accepted.
pub const MAX_READ_AUTH_HEADER_LEN: usize = 1024;
/// Longest `agent_id` or `key_id` (decoded bytes) accepted in the header.
pub const MAX_READ_AUTH_ID_LEN: usize = 256;
/// Nonce length bounds in hex characters.
pub const READ_AUTH_NONCE_HEX: std::ops::RangeInclusive<usize> = 16..=64;

/// Why a read proof was refused. Variants carry no secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadAuthError {
    /// The header does not follow the grammar.
    Malformed(&'static str),
    /// The timestamp is outside [`READ_AUTH_WINDOW_SECS`] of the verifier clock.
    Stale {
        /// Seconds between the proof timestamp and the verifier clock.
        skew_secs: i64,
    },
    /// The signature does not verify for this request.
    BadSignature,
    /// The nonce was already used inside the window.
    Replayed,
    /// The nonce cache is full of unexpired entries; try again shortly.
    CacheFull,
}

impl fmt::Display for ReadAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed read proof: {why}"),
            Self::Stale { skew_secs } => write!(
                f,
                "read proof timestamp is {skew_secs}s from the server clock (limit {READ_AUTH_WINDOW_SECS}s)"
            ),
            Self::BadSignature => f.write_str("read proof signature does not verify"),
            Self::Replayed => f.write_str("read proof was already used"),
            Self::CacheFull => f.write_str("read proof replay cache is full; retry shortly"),
        }
    }
}

impl std::error::Error for ReadAuthError {}

/// A parsed read-proof header.
///
/// Parsing is strict and the rendering is canonical: a header is accepted only
/// if [`ReadAuthHeader::to_header_value`] reproduces it byte for byte.
///
/// # Examples
///
/// ```
/// use mentisdb::read_auth::ReadAuthHeader;
///
/// let header = ReadAuthHeader {
///     agent_id: "alice smith".into(),
///     key_id: "k/1".into(),
///     public_key: [3; 32],
///     timestamp: 1_700_000_000,
///     nonce: "ab".repeat(16),
///     signature: [9; 64],
/// };
/// let text = header.to_header_value();
/// assert!(text.starts_with("v1 agent=alice%20smith;key=k%2F1;pk="));
/// assert_eq!(ReadAuthHeader::parse(&text).unwrap(), header);
/// assert!(ReadAuthHeader::parse("v2 anything").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAuthHeader {
    /// Agent the key belongs to, as listed in the chain's access policy.
    pub agent_id: String,
    /// Id of the key within that agent.
    pub key_id: String,
    /// Raw Ed25519 public key.
    pub public_key: [u8; 32],
    /// Unix seconds the request was signed at.
    pub timestamp: i64,
    /// Random value, 16 to 64 lowercase hex characters, never reused.
    pub nonce: String,
    /// Ed25519 signature over [`read_auth_payload`].
    pub signature: [u8; 64],
}

fn pct_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn pct_decode(text: &str) -> Result<String, ReadAuthError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .ok_or(ReadAuthError::Malformed("truncated percent escape"))?;
                let digit = |b: u8| match b {
                    b'0'..=b'9' => Ok(b - b'0'),
                    b'A'..=b'F' => Ok(b - b'A' + 10),
                    _ => Err(ReadAuthError::Malformed("bad percent escape")),
                };
                out.push(digit(hex[0])? << 4 | digit(hex[1])?);
                i += 3;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-') => {
                out.push(byte);
                i += 1;
            }
            _ => return Err(ReadAuthError::Malformed("unescaped character in id")),
        }
    }
    String::from_utf8(out).map_err(|_| ReadAuthError::Malformed("id is not UTF-8"))
}

impl ReadAuthHeader {
    /// Render the canonical header value.
    #[must_use]
    pub fn to_header_value(&self) -> String {
        format!(
            "v1 agent={};key={};pk={};ts={};nonce={};sig={}",
            pct_encode(&self.agent_id),
            pct_encode(&self.key_id),
            to_hex(&self.public_key),
            self.timestamp,
            self.nonce,
            to_hex(&self.signature)
        )
    }

    /// Parse a header value. Does not check the signature or the clock; see
    /// [`verify_request`].
    ///
    /// # Errors
    ///
    /// [`ReadAuthError::Malformed`] for anything that is not exactly the
    /// canonical form: wrong version, missing, repeated, reordered or unknown
    /// field, bad hex, overlong value, or a timestamp that does not round-trip.
    pub fn parse(value: &str) -> Result<Self, ReadAuthError> {
        use ReadAuthError::Malformed;
        if value.len() > MAX_READ_AUTH_HEADER_LEN {
            return Err(Malformed("header too long"));
        }
        let rest = value
            .strip_prefix("v1 ")
            .ok_or(Malformed("unsupported version"))?;
        let mut fields = rest.split(';');
        let mut take = |name: &'static str| -> Result<&str, ReadAuthError> {
            fields
                .next()
                .and_then(|field| field.strip_prefix(name))
                .and_then(|field| field.strip_prefix('='))
                .ok_or(Malformed(
                    "fields must be agent, key, pk, ts, nonce, sig in order",
                ))
        };
        let agent_id = pct_decode(take("agent")?)?;
        let key_id = pct_decode(take("key")?)?;
        let pk_hex = take("pk")?;
        let ts_text = take("ts")?;
        let nonce = take("nonce")?.to_string();
        let sig_hex = take("sig")?;
        if fields.next().is_some() {
            return Err(Malformed("trailing data"));
        }
        if agent_id.is_empty() || key_id.is_empty() {
            return Err(Malformed("agent and key must not be empty"));
        }
        if agent_id.len() > MAX_READ_AUTH_ID_LEN || key_id.len() > MAX_READ_AUTH_ID_LEN {
            return Err(Malformed("agent or key too long"));
        }
        let hex_ok = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex_ok(pk_hex) || !hex_ok(sig_hex) || !hex_ok(&nonce) {
            return Err(Malformed("hex fields must be lowercase hex"));
        }
        let public_key: [u8; 32] = from_hex(pk_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(Malformed("pk must be 32 bytes"))?;
        let signature: [u8; 64] = from_hex(sig_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(Malformed("sig must be 64 bytes"))?;
        if !READ_AUTH_NONCE_HEX.contains(&nonce.len()) {
            return Err(Malformed("nonce must be 16 to 64 hex characters"));
        }
        let timestamp: i64 = ts_text.parse().map_err(|_| Malformed("bad ts"))?;
        let header = Self {
            agent_id,
            key_id,
            public_key,
            timestamp,
            nonce,
            signature,
        };
        if header.to_header_value() != value {
            return Err(Malformed("not in canonical form"));
        }
        Ok(header)
    }
}

/// The bytes a read proof signs.
///
/// `method` is upper-cased; `target` is the request path plus `?query` exactly as
/// sent; `body` is the full request body (empty for `GET`). The identity,
/// timestamp and nonce come from `header`; its signature is not part of the
/// preimage.
#[must_use]
pub fn read_auth_payload(
    method: &str,
    target: &str,
    body: &[u8],
    header: &ReadAuthHeader,
) -> Vec<u8> {
    let mut p = Payload::new(READ_AUTH_DOMAIN);
    p.str(&method.to_ascii_uppercase()).str(target);
    p.raw(&Sha256::digest(body));
    p.str(&header.agent_id)
        .str(&header.key_id)
        .raw(&header.public_key);
    p.i64(header.timestamp).str(&header.nonce);
    p.0
}

/// A fresh random nonce: 32 lowercase hex characters from a UUIDv4 (the
/// operating system's CSPRNG).
#[must_use]
pub fn fresh_nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Sign a request and return the header value to send.
///
/// `seed` is the 32-byte Ed25519 seed. It stays in the calling process.
///
/// # Panics
///
/// Panics if `agent_id` or `key_id` is empty or `nonce` is not 16 to 64
/// lowercase hex characters (use [`fresh_nonce`]); a header built from them
/// could never verify.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn sign_request(
    seed: &[u8; 32],
    agent_id: &str,
    key_id: &str,
    method: &str,
    target: &str,
    body: &[u8],
    timestamp: i64,
    nonce: &str,
) -> String {
    assert!(!agent_id.is_empty() && !key_id.is_empty(), "empty id");
    assert!(
        READ_AUTH_NONCE_HEX.contains(&nonce.len())
            && nonce
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "nonce must be 16 to 64 lowercase hex characters"
    );
    let secret = SigningKey::from_bytes(seed);
    let mut header = ReadAuthHeader {
        agent_id: agent_id.to_string(),
        key_id: key_id.to_string(),
        public_key: secret.verifying_key().to_bytes(),
        timestamp,
        nonce: nonce.to_string(),
        signature: [0; 64],
    };
    header.signature = secret
        .sign(&read_auth_payload(method, target, body, &header))
        .to_bytes();
    header.to_header_value()
}

/// Verify a header against the request it arrived on.
///
/// Checks the grammar, the clock window and the signature (strict Ed25519).
/// It does **not** check replay or authorization: pass the result to
/// [`NonceCache::check_and_insert`] and then to the chain's authorized key set.
///
/// # Errors
///
/// [`ReadAuthError::Malformed`], [`ReadAuthError::Stale`] or
/// [`ReadAuthError::BadSignature`].
pub fn verify_request(
    header_value: &str,
    method: &str,
    target: &str,
    body: &[u8],
    now_unix_secs: i64,
) -> Result<ReadAuthHeader, ReadAuthError> {
    let header = ReadAuthHeader::parse(header_value)?;
    let skew_secs = now_unix_secs.saturating_sub(header.timestamp);
    if skew_secs.unsigned_abs() > READ_AUTH_WINDOW_SECS.unsigned_abs() {
        return Err(ReadAuthError::Stale { skew_secs });
    }
    let payload = read_auth_payload(method, target, body, &header);
    verify_ed25519_signature(&header.public_key, &payload, &header.signature)
        .map_err(|_| ReadAuthError::BadSignature)?;
    Ok(header)
}

/// Remembers recently used `(key, nonce)` pairs so a proof works once.
///
/// Entries live for twice [`READ_AUTH_WINDOW_SECS`] (a proof is valid for a
/// window on each side of the clock), and the cache is bounded so a flood of
/// valid proofs cannot exhaust memory: when full of live entries it refuses
/// new ones ([`ReadAuthError::CacheFull`]) and the requests fall back to the
/// public view.
///
/// # Examples
///
/// ```
/// use mentisdb::read_auth::{NonceCache, ReadAuthError};
///
/// let cache = NonceCache::new(8);
/// assert!(cache.check_and_insert(&[1; 32], "aaaaaaaaaaaaaaaa", 100).is_ok());
/// assert_eq!(cache.check_and_insert(&[1; 32], "aaaaaaaaaaaaaaaa", 101), Err(ReadAuthError::Replayed));
/// // Same nonce under another key is a different entry.
/// assert!(cache.check_and_insert(&[2; 32], "aaaaaaaaaaaaaaaa", 101).is_ok());
/// ```
#[derive(Debug)]
pub struct NonceCache {
    capacity: usize,
    seen: Mutex<HashMap<[u8; 32], i64>>,
}

impl NonceCache {
    /// Create a cache holding at most `capacity` live entries.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Record `(public_key, nonce)` as used at `now_unix_secs`.
    ///
    /// # Errors
    ///
    /// [`ReadAuthError::Replayed`] when the pair is already live;
    /// [`ReadAuthError::CacheFull`] when the cache holds `capacity` live entries.
    pub fn check_and_insert(
        &self,
        public_key: &[u8; 32],
        nonce: &str,
        now_unix_secs: i64,
    ) -> Result<(), ReadAuthError> {
        let mut hasher = Sha256::new();
        hasher.update(public_key);
        hasher.update(nonce.as_bytes());
        let id: [u8; 32] = hasher.finalize().into();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(expires) = seen.get(&id) {
            if *expires > now_unix_secs {
                return Err(ReadAuthError::Replayed);
            }
        }
        if seen.len() >= self.capacity {
            seen.retain(|_, expires| *expires > now_unix_secs);
            if seen.len() >= self.capacity {
                return Err(ReadAuthError::CacheFull);
            }
        }
        seen.insert(id, now_unix_secs.saturating_add(2 * READ_AUTH_WINDOW_SECS));
        Ok(())
    }

    /// Number of entries currently stored (live or expired but not yet pruned).
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// `true` when nothing is stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Environment variable naming the seed file used to prove read access.
pub const ENV_READ_KEY_FILE: &str = "MENTISDB_READ_KEY_FILE";
/// Environment variable naming the agent of the read key.
pub const ENV_READ_AGENT_ID: &str = "MENTISDB_READ_AGENT_ID";
/// Environment variable naming the key id of the read key.
pub const ENV_READ_KEY_ID: &str = "MENTISDB_READ_KEY_ID";

/// A key a client process holds to sign read proofs.
///
/// The seed lives in this process only. The stdio MCP proxy and the CLI build one
/// from the environment and sign every request they forward, so the language
/// model on the other side of the proxy never sees the key, only the answers.
///
/// # Examples
///
/// ```
/// use mentisdb::read_auth::{verify_request, ReadSigner};
///
/// let signer = ReadSigner::new([5u8; 32], "alice", "alice-1");
/// let header = signer.header("POST", "/", b"{}");
/// assert!(verify_request(&header, "POST", "/", b"{}", chrono::Utc::now().timestamp()).is_ok());
/// ```
#[derive(Clone)]
pub struct ReadSigner {
    seed: [u8; 32],
    agent_id: String,
    key_id: String,
}

impl std::fmt::Debug for ReadSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReadSigner({}/{})", self.agent_id, self.key_id)
    }
}

impl ReadSigner {
    /// Build a signer from a seed and the identity the chain's policy lists.
    #[must_use]
    pub fn new(seed: [u8; 32], agent_id: impl Into<String>, key_id: impl Into<String>) -> Self {
        Self {
            seed,
            agent_id: agent_id.into(),
            key_id: key_id.into(),
        }
    }

    /// Build a signer from `MENTISDB_READ_KEY_FILE`, `MENTISDB_READ_AGENT_ID` and
    /// `MENTISDB_READ_KEY_ID`; `Ok(None)` when none of them is set.
    ///
    /// # Errors
    ///
    /// A message when the file is set but unreadable or not a 32-byte hex seed, or
    /// when the agent or key id is missing.
    pub fn from_env() -> Result<Option<Self>, String> {
        let get = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let Some(file) = get(ENV_READ_KEY_FILE) else {
            return Ok(None);
        };
        let (Some(agent_id), Some(key_id)) = (get(ENV_READ_AGENT_ID), get(ENV_READ_KEY_ID)) else {
            return Err(format!(
                "{ENV_READ_KEY_FILE} also needs {ENV_READ_AGENT_ID} and {ENV_READ_KEY_ID}"
            ));
        };
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("cannot read '{file}': {e}"))?;
        let seed: [u8; 32] = from_hex(text.trim())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| format!("'{file}' must hold a 32-byte seed as 64 hex characters"))?;
        Ok(Some(Self::new(seed, agent_id, key_id)))
    }

    /// The header value that authenticates one request.
    #[must_use]
    pub fn header(&self, method: &str, target: &str, body: &[u8]) -> String {
        sign_request(
            &self.seed,
            &self.agent_id,
            &self.key_id,
            method,
            target,
            body,
            chrono::Utc::now().timestamp(),
            &fresh_nonce(),
        )
    }
}
