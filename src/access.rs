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

use crate::signing::{SignatureVersion, SigningError};
use std::fmt;
use std::io;

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
