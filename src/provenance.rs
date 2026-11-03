//! How far a stored thought's authorship can be trusted.
//!
//! Every thought names the agent that wrote it, but on a chain that accepts
//! unsigned writes that name is only a claim: anyone who can reach the API can
//! write as any agent. A thought that carries a signature checked against the
//! agent's registered key is different, because the writer had to hold the
//! private key. [`ProvenanceTrust`] is that distinction as a value, so the
//! prompts and responses that carry memory to an agent can show it next to the
//! text instead of leaving the agent to guess.
//!
//! It says nothing about whether the *content* is true or safe to follow.
//! Stored thoughts are always data to weigh, never instructions (see
//! `docs/CHAIN_ACCESS.md`, *Provenance and the prompt-injection boundary*).
//!
//! # Examples
//!
//! ```
//! use mentisdb::provenance::ProvenanceTrust;
//! use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};
//!
//! # fn main() -> std::io::Result<()> {
//! let dir = tempfile::tempdir()?;
//! let mut chain = MentisDb::open_with_key_and_storage_kind(dir.path(), "demo", StorageAdapterKind::Binary)?;
//! let thought = chain.append("alice", ThoughtType::Constraint, "Never deploy on Fridays.")?.clone();
//! assert_eq!(chain.trust_of(&thought), ProvenanceTrust::Unsigned);
//! assert_eq!(ProvenanceTrust::Unsigned.as_str(), "unsigned");
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};

/// Whether a thought's author is established by a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTrust {
    /// The thought carries a signature that verifies against an active key
    /// registered to its agent: the writer held that key.
    Verified,
    /// No signature. `agent_id` is only what the writer claimed.
    #[default]
    Unsigned,
    /// A signature is present but does not verify, names a key that is not
    /// registered, or the key (or its agent) has since been revoked.
    Unverified,
}

impl ProvenanceTrust {
    /// Lowercase label used in prompts and JSON (`verified`, `unsigned`,
    /// `unverified`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unsigned => "unsigned",
            Self::Unverified => "unverified",
        }
    }

    /// `true` only for [`ProvenanceTrust::Verified`].
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }
}

impl std::fmt::Display for ProvenanceTrust {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
