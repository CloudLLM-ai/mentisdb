use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;
use uuid::Uuid;

/// First supported version of the persisted skill registry file.
pub const MENTISDB_SKILL_REGISTRY_V1: u32 = 1;
/// Second version of the persisted skill registry file — introduces delta/diff content storage and optional signing.
pub const MENTISDB_SKILL_REGISTRY_V2: u32 = 2;
/// Alias for the current persisted skill registry file version.
pub const MENTISDB_SKILL_REGISTRY_CURRENT_VERSION: u32 = MENTISDB_SKILL_REGISTRY_V2;
/// First supported version of the structured skill schema.
pub const MENTISDB_SKILL_SCHEMA_V1: u32 = 1;
/// Alias for the current structured skill schema version.
pub const MENTISDB_SKILL_CURRENT_SCHEMA_VERSION: u32 = MENTISDB_SKILL_SCHEMA_V1;
const MENTISDB_SKILL_REGISTRY_FILENAME: &str = "mentisdb-skills.bin";

/// Supported import and export formats for skill documents.
///
/// MentisDB stores every skill as a [`SkillVersionContent::Full`] raw string in the
/// format it was uploaded in.  The registry can round-trip any skill through either
/// format: upload in Markdown, read back as JSON (or vice-versa) via
/// [`SkillRegistry::read_skill`].
///
/// # Examples
///
/// ```rust
/// use mentisdb::SkillFormat;
/// use std::str::FromStr;
///
/// // Display gives the canonical lowercase name.
/// assert_eq!(SkillFormat::Markdown.to_string(), "markdown");
/// assert_eq!(SkillFormat::Json.to_string(), "json");
///
/// // Both "md" and "markdown" parse as Markdown.
/// assert_eq!("md".parse::<SkillFormat>().unwrap(), SkillFormat::Markdown);
/// assert_eq!("json".parse::<SkillFormat>().unwrap(), SkillFormat::Json);
///
/// // Unknown strings return an error.
/// assert!("xml".parse::<SkillFormat>().is_err());
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillFormat {
    /// Markdown skill document with optional YAML-like frontmatter.
    ///
    /// The Markdown parser recognises an optional `---` … `---` frontmatter block
    /// at the top of the file that may carry `name`, `description`, `tags`,
    /// `triggers`, and `warnings` key-value pairs.  Everything after the
    /// frontmatter block is parsed into [`SkillSection`]s by heading level.
    Markdown,
    /// JSON representation of the structured [`SkillDocument`] object.
    ///
    /// The JSON format is a direct `serde_json` serialisation of [`SkillDocument`].
    /// It is useful for programmatic consumption or when the skill content is
    /// already available as a structured object rather than freeform Markdown.
    Json,
}

impl SkillFormat {
    /// Return the stable lowercase name of this format.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Json => "json",
        }
    }
}

impl fmt::Display for SkillFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "markdown" | "md" => Ok(Self::Markdown),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "Unsupported skill format \'{other}\'. Expected \'markdown\' or \'json\'"
            )),
        }
    }
}

/// Lifecycle state of a stored skill entry.
///
/// Every skill begins life as [`SkillStatus::Active`].  Registry operators can
/// advance the lifecycle to [`SkillStatus::Deprecated`] or
/// [`SkillStatus::Revoked`] via [`SkillRegistry::deprecate_skill`] and
/// [`SkillRegistry::revoke_skill`].  **Neither transition deletes any version
/// history** — the registry is append-only and all prior versions remain
/// accessible.
///
/// | Status       | Searchable | Safe to use | Indicates                              |
/// |--------------|-----------|-------------|----------------------------------------|
/// | `Active`     | ✅        | ✅          | Normal operational skill               |
/// | `Deprecated` | ✅        | ⚠️          | Superseded; prefer a newer alternative |
/// | `Revoked`    | ✅        | ❌          | Safety/correctness concern             |
///
/// # Examples
///
/// ```rust
/// use mentisdb::SkillStatus;
/// use std::str::FromStr;
///
/// // Display gives the canonical lowercase name.
/// assert_eq!(SkillStatus::Active.to_string(), "active");
/// assert_eq!(SkillStatus::Deprecated.to_string(), "deprecated");
/// assert_eq!(SkillStatus::Revoked.to_string(), "revoked");
///
/// // "disabled" is accepted as an alias for "revoked".
/// assert_eq!("disabled".parse::<SkillStatus>().unwrap(), SkillStatus::Revoked);
///
/// // Unknown strings return an error.
/// assert!("unknown".parse::<SkillStatus>().is_err());
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillStatus {
    /// The skill is active and in normal use.
    ///
    /// `Active` is the default status assigned when a skill is first uploaded,
    /// and it is automatically restored on every subsequent upload (unless the
    /// skill is currently [`Revoked`](SkillStatus::Revoked)).  Skills in this
    /// state are returned by all search and list calls.
    Active,
    /// The skill has been superseded but is still safe to read.
    ///
    /// Use `Deprecated` when a newer version of the skill (possibly under a
    /// different `skill_id`) should be preferred.  Deprecated skills remain
    /// fully accessible and searchable; callers are expected to surface the
    /// deprecation reason to users or agents.
    Deprecated,
    /// The skill must not be used; a safety or correctness concern was found.
    ///
    /// Revoked skills are still stored (all version history is preserved) but
    /// agents and callers should treat them as untrusted.  A subsequent upload
    /// to the same `skill_id` will **not** clear the `Revoked` status — use
    /// a new `skill_id` to publish a corrected replacement.
    Revoked,
}

impl SkillStatus {
    /// Return the stable lowercase name of this status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Revoked => "revoked",
        }
    }
}

impl fmt::Display for SkillStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "deprecated" => Ok(Self::Deprecated),
            "revoked" | "disabled" => Ok(Self::Revoked),
            other => Err(format!(
                "Unsupported skill status \'{other}\'. Expected \'active\', \'deprecated\', or \'revoked\'"
            )),
        }
    }
}

/// One heading-delimited section of a structured skill document.
///
/// The Markdown parser splits a skill file into sections at each heading
/// (`#` through `######`).  Every run of non-heading lines that follows a
/// heading becomes the section's `body`.  Lines that appear before the first
/// heading are used as the document-level `description` in [`SkillDocument`].
///
/// # Examples
///
/// ```rust
/// use mentisdb::{import_skill, SkillFormat, SkillSection};
///
/// let md = "# Memory Management\n\nAlways clean up allocations.\n\n## Tips\n\nUse RAII.";
/// let doc = import_skill(md, SkillFormat::Markdown).unwrap();
///
/// assert_eq!(doc.sections[0].level, 1);
/// assert_eq!(doc.sections[0].heading, "Memory Management");
/// assert_eq!(doc.sections[1].level, 2);
/// assert_eq!(doc.sections[1].heading, "Tips");
/// assert!(doc.sections[1].body.contains("RAII"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSection {
    /// Markdown heading level, from `1` (`#`) through `6` (`######`).
    pub level: u8,
    /// Section heading text without the leading `#` markers.
    pub heading: String,
    /// Section body text.
    pub body: String,
}

/// Structured representation of a parsed skill file.
///
/// A `SkillDocument` is the canonical in-memory form of a skill.  It is
/// produced by [`import_skill`] from raw Markdown or JSON source and consumed
/// by [`export_skill`] to render back to either format.  The [`SkillRegistry`]
/// stores skills as raw text and reconstructs `SkillDocument` on demand so
/// that the schema can evolve without re-encoding all stored versions.
///
/// ## Frontmatter fields
///
/// When importing from Markdown, the following YAML-like frontmatter keys are
/// recognised (all optional):
///
/// | Key              | Mapped field  |
/// |------------------|---------------|
/// | `name`           | `name`        |
/// | `description`    | `description` |
/// | `tags`           | `tags`        |
/// | `triggers`       | `triggers`    |
/// | `warnings`       | `warnings`    |
/// | `schema_version` | `schema_version` |
///
/// If `name` is absent from frontmatter the first `#`-level heading is used.
/// If `description` is absent, any text that appears before the first heading
/// (the "intro" block) is used.
///
/// # Examples
///
/// ```rust
/// use mentisdb::{import_skill, export_skill, SkillFormat};
///
/// // Markdown with frontmatter — name and description come from the header block.
/// // Section headings use ## (level 2) so they remain intact in doctest compilation.
/// let markdown = r#"---
/// name: Task Planner
/// description: Break large goals into ordered sub-tasks.
/// tags: [planning, tasks]
/// triggers: [plan this, decompose]
/// ---
///
/// ## Overview
///
/// Useful for project planning and task decomposition.
///
/// ## Steps
///
/// 1. Identify the goal.
/// 2. List sub-tasks.
/// 3. Order by dependency.
/// "#;
///
/// let doc = import_skill(markdown, SkillFormat::Markdown).unwrap();
/// assert_eq!(doc.name, "Task Planner");
/// assert!(doc.tags.contains(&"planning".to_string()));
/// assert_eq!(doc.sections[0].heading, "Overview");
/// assert_eq!(doc.sections[1].heading, "Steps");
///
/// // Round-trip to JSON and back preserves all fields.
/// let json = export_skill(&doc, SkillFormat::Json).unwrap();
/// let doc2 = import_skill(&json, SkillFormat::Json).unwrap();
/// assert_eq!(doc, doc2);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDocument {
    /// Schema version for this structured skill object.
    pub schema_version: u32,
    /// Stable skill name from frontmatter or the first heading.
    pub name: String,
    /// Short description of when and why to use the skill.
    pub description: String,
    /// Optional retrieval tags for the skill registry.
    pub tags: Vec<String>,
    /// Optional trigger phrases or domains that should suggest this skill.
    pub triggers: Vec<String>,
    /// Optional warnings to show before an agent trusts or executes the skill.
    pub warnings: Vec<String>,
    /// Ordered Markdown sections making up the body of the skill.
    pub sections: Vec<SkillSection>,
}

impl SkillDocument {
    fn validate(&self) -> io::Result<()> {
        if self.schema_version == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "skill schema_version must be greater than zero",
            ));
        }
        if self.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "skill name must not be empty",
            ));
        }
        if self.description.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "skill description must not be empty",
            ));
        }
        Ok(())
    }
}

/// The stored content form for a single skill version.
///
/// The first version of a skill is always stored in [`SkillVersionContent::Full`] form.
/// Subsequent versions store only the unified diff [`SkillVersionContent::Delta`] against
/// the immediately preceding version, saving storage for iterative improvements.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillVersionContent {
    /// Complete raw source text; used for the first version of every skill.
    Full {
        /// The complete raw skill source (Markdown or JSON).
        raw: String,
    },
    /// Unified diff patch against the immediately preceding version.
    Delta {
        /// A unified diff patch string produced by `diffy::create_patch`.
        patch: String,
    },
}

/// One immutable uploaded skill version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillVersion {
    /// Stable unique version identifier.
    pub version_id: Uuid,
    /// Zero-based monotone version number within this skill\'s history.
    pub version_number: u32,
    /// UTC timestamp when this version was uploaded.
    pub uploaded_at: DateTime<Utc>,
    /// Stable agent identifier responsible for the upload.
    pub uploaded_by_agent_id: String,
    /// Optional human-readable agent name from the agent registry.
    pub uploaded_by_agent_name: Option<String>,
    /// Optional agent owner or tenant label from the agent registry.
    pub uploaded_by_agent_owner: Option<String>,
    /// Original input format used during upload.
    pub source_format: SkillFormat,
    /// SHA-256 hex digest of the full reconstructed raw content for this version.
    pub content_hash: String,
    /// Stored content — either the full raw text or a delta patch.
    pub content: SkillVersionContent,
    /// The `key_id` of the agent key used to sign this upload, if any.
    pub signing_key_id: Option<String>,
    /// Ed25519 signature bytes over the raw content, if any.
    pub skill_signature: Option<Vec<u8>>,
}

/// One skill entry containing immutable uploaded versions plus lifecycle status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEntry {
    /// Stable skill identifier used for reads, searches, and version listing.
    pub skill_id: String,
    /// UTC timestamp when this skill id first appeared in the registry.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp when the latest version or lifecycle update was applied.
    pub updated_at: DateTime<Utc>,
    /// Current skill lifecycle status.
    pub status: SkillStatus,
    /// Optional deprecation or revocation reason.
    pub status_reason: Option<String>,
    /// Immutable uploaded versions in chronological order.
    pub versions: Vec<SkillVersion>,
}

impl SkillEntry {
    fn latest_version(&self) -> &SkillVersion {
        self.versions
            .last()
            .expect("skill entry must always contain at least one version")
    }
}

/// Lightweight searchable summary of one stored skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSummary {
    /// Stable skill identifier.
    pub skill_id: String,
    /// Latest skill name.
    pub name: String,
    /// Latest skill description.
    pub description: String,
    /// Current lifecycle status.
    pub status: SkillStatus,
    /// Optional deprecation or revocation reason.
    pub status_reason: Option<String>,
    /// Latest uploaded skill schema version.
    pub schema_version: u32,
    /// Latest uploaded tags.
    pub tags: Vec<String>,
    /// Latest uploaded trigger phrases.
    pub triggers: Vec<String>,
    /// Latest uploaded warnings.
    pub warnings: Vec<String>,
    /// Stable id of the latest version for `read_skill`.
    pub latest_version_id: Uuid,
    /// Total number of uploaded versions.
    pub version_count: usize,
    /// UTC timestamp when the skill was created.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp when the latest version or lifecycle change was applied.
    pub updated_at: DateTime<Utc>,
    /// UTC timestamp when the latest version was uploaded.
    pub latest_uploaded_at: DateTime<Utc>,
    /// Responsible agent id for the latest version.
    pub latest_uploaded_by_agent_id: String,
    /// Responsible agent name for the latest version, if known.
    pub latest_uploaded_by_agent_name: Option<String>,
    /// Responsible agent owner for the latest version, if known.
    pub latest_uploaded_by_agent_owner: Option<String>,
    /// Original format of the latest uploaded version.
    pub latest_source_format: SkillFormat,
}

/// Lightweight summary of one immutable skill version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillVersionSummary {
    /// Stable skill identifier.
    pub skill_id: String,
    /// Stable unique version identifier.
    pub version_id: Uuid,
    /// Zero-based monotone version number within this skill\'s history.
    pub version_number: u32,
    /// UTC timestamp when this version was uploaded.
    pub uploaded_at: DateTime<Utc>,
    /// Responsible agent id.
    pub uploaded_by_agent_id: String,
    /// Responsible agent name, if known.
    pub uploaded_by_agent_name: Option<String>,
    /// Responsible agent owner, if known.
    pub uploaded_by_agent_owner: Option<String>,
    /// Original input format for this version.
    pub source_format: SkillFormat,
    /// Structured skill schema version for this version.
    pub schema_version: u32,
    /// Content hash of this version.
    pub content_hash: String,
    /// The `key_id` of the agent key used to sign this version, if any.
    pub signing_key_id: Option<String>,
}

/// Upload request for one new immutable skill version.
///
/// `SkillUpload` is a builder-style value that groups the upload metadata
/// consumed by [`SkillRegistry::upload_skill`].  Required fields are provided
/// to [`SkillUpload::new`]; optional fields are attached with the chainable
/// builder methods.
///
/// ## Required fields
///
/// | Field                  | Source                    |
/// |------------------------|---------------------------|
/// | `uploaded_by_agent_id` | The stable agent identifier responsible for this upload |
/// | `format`               | The [`SkillFormat`] of the raw `content` string        |
/// | `content`              | The full raw Markdown or JSON skill source              |
///
/// ## Optional fields (builder methods)
///
/// | Builder method        | What it sets                                          |
/// |-----------------------|-------------------------------------------------------|
/// | `with_skill_id`       | Explicit stable id; derived from name if omitted      |
/// | `with_agent_identity` | Human-readable agent name and owner for display       |
/// | `with_signing`        | Ed25519 signing key id and raw signature bytes        |
///
/// # Examples
///
/// ```rust
/// use mentisdb::{SkillUpload, SkillFormat};
///
/// // Minimal upload — skill_id derived from the skill name in the content.
/// let minimal = SkillUpload::new(
///     "agent-42",
///     SkillFormat::Markdown,
///     "# Code Review\n\nReview PRs for correctness.",
/// );
///
/// // Full upload with explicit skill_id, display identity, and (placeholder) signature.
/// let full = SkillUpload::new(
///     "agent-42",
///     SkillFormat::Markdown,
///     "# Code Review\n\nReview PRs for correctness.",
/// )
/// .with_skill_id("code-review")
/// .with_agent_identity(Some("Reviewer Agent"), Some("acme-corp"))
/// .with_signing(Some("key-v1".to_string()), Some(vec![0u8; 64]));
/// ```
#[derive(Debug, Clone)]
pub struct SkillUpload<'a> {
    skill_id: Option<&'a str>,
    uploaded_by_agent_id: &'a str,
    uploaded_by_agent_name: Option<&'a str>,
    uploaded_by_agent_owner: Option<&'a str>,
    format: SkillFormat,
    content: &'a str,
    signing_key_id: Option<String>,
    skill_signature: Option<Vec<u8>>,
}

impl<'a> SkillUpload<'a> {
    /// Create a new upload request with the required fields.
    ///
    /// The `skill_id` will be derived from the skill name parsed out of `content`
    /// unless overridden with [`SkillUpload::with_skill_id`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{SkillUpload, SkillFormat};
    ///
    /// let upload = SkillUpload::new(
    ///     "my-agent",
    ///     SkillFormat::Markdown,
    ///     "# Hello\n\nA minimal skill.",
    /// );
    /// ```
    pub fn new(uploaded_by_agent_id: &'a str, format: SkillFormat, content: &'a str) -> Self {
        Self {
            skill_id: None,
            uploaded_by_agent_id,
            uploaded_by_agent_name: None,
            uploaded_by_agent_owner: None,
            format,
            content,
            signing_key_id: None,
            skill_signature: None,
        }
    }

    /// Attach an explicit skill id instead of deriving one from the skill name.
    ///
    /// The provided value is normalised to a URL-safe slug (lowercase
    /// alphanumerics and hyphens).  Use this when you need a stable, predictable
    /// id regardless of how the skill name is worded.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{SkillUpload, SkillFormat};
    ///
    /// let upload = SkillUpload::new("agent-1", SkillFormat::Markdown, "# My Skill\n\nDoes things.")
    ///     .with_skill_id("my-skill");
    /// ```
    pub fn with_skill_id(mut self, skill_id: &'a str) -> Self {
        self.skill_id = Some(skill_id);
        self
    }

    /// Attach optional human-readable agent identity metadata.
    ///
    /// Both parameters are trimmed and stored as-is; `None` or blank values are
    /// treated as absent.  These fields appear in [`SkillSummary`] and
    /// [`SkillVersionSummary`] for display purposes and are searchable via
    /// [`SkillQuery::uploaded_by_agent_names`] and
    /// [`SkillQuery::uploaded_by_agent_owners`].
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{SkillUpload, SkillFormat};
    ///
    /// let upload = SkillUpload::new("agent-7", SkillFormat::Markdown, "# Demo\n\nDemonstration skill.")
    ///     .with_agent_identity(Some("Helpful Agent"), Some("acme-org"));
    /// ```
    pub fn with_agent_identity(
        mut self,
        uploaded_by_agent_name: Option<&'a str>,
        uploaded_by_agent_owner: Option<&'a str>,
    ) -> Self {
        self.uploaded_by_agent_name = uploaded_by_agent_name;
        self.uploaded_by_agent_owner = uploaded_by_agent_owner;
        self
    }

    /// Attach optional signing metadata for this upload.
    ///
    /// When provided, `signing_key_id` identifies the Ed25519 key registered on
    /// the uploading agent, and `skill_signature` contains the raw 64-byte
    /// signature over the skill content.  The registry stores both values
    /// verbatim in the [`SkillVersion`]; it does **not** verify the signature
    /// during upload — verification is the caller's responsibility.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{SkillUpload, SkillFormat};
    ///
    /// // In production use a real Ed25519 signature; here we show the shape.
    /// let fake_sig = vec![0u8; 64];
    /// let upload = SkillUpload::new("agent-1", SkillFormat::Markdown, "# Signed\n\nSigned skill.")
    ///     .with_signing(Some("key-2024".to_string()), Some(fake_sig));
    /// ```
    pub fn with_signing(
        mut self,
        signing_key_id: Option<String>,
        skill_signature: Option<Vec<u8>>,
    ) -> Self {
        self.signing_key_id = signing_key_id;
        self.skill_signature = skill_signature;
        self
    }
}

/// Query parameters for [`SkillRegistry::search_skills`].
///
/// Every field is optional and acts as an independent filter.  When multiple
/// fields are set the registry returns only skills that satisfy **all** of them
/// (logical AND across fields).  Within a single multi-value field (e.g.
/// `tags_any`) the registry returns skills that match **any** of the values
/// (logical OR within a field).
///
/// All string comparisons are case-insensitive.  The `text` filter is applied
/// last, after all indexed filters have been evaluated, and it searches across
/// the skill name, description, warnings, section headings, and section bodies.
///
/// ## Builder-style construction
///
/// `SkillQuery` derives [`Default`] so the idiomatic pattern is to start with
/// `SkillQuery::default()` and set only the fields you need:
///
/// ```rust
/// use mentisdb::{SkillQuery, SkillStatus, SkillFormat};
///
/// // Find active skills tagged with "memory" or "cache" that mention "evict".
/// let query = SkillQuery {
///     text: Some("evict".into()),
///     tags_any: vec!["memory".into(), "cache".into()],
///     statuses: Some(vec![SkillStatus::Active]),
///     ..Default::default()
/// };
///
/// // Limit to the 5 most recently updated results.
/// let limited = SkillQuery {
///     formats: Some(vec![SkillFormat::Markdown]),
///     limit: Some(5),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillQuery {
    /// Optional text filter applied to latest name, description, warnings, headings, and bodies.
    pub text: Option<String>,
    /// Optional skill ids to match.
    pub skill_ids: Option<Vec<String>>,
    /// Optional exact skill names to match.
    pub names: Option<Vec<String>>,
    /// Optional tags to match.
    pub tags_any: Vec<String>,
    /// Optional trigger phrases to match.
    pub triggers_any: Vec<String>,
    /// Optional uploader agent ids to match across any version.
    pub uploaded_by_agent_ids: Option<Vec<String>>,
    /// Optional uploader agent display names to match across any version.
    pub uploaded_by_agent_names: Option<Vec<String>>,
    /// Optional uploader agent owner labels to match across any version.
    pub uploaded_by_agent_owners: Option<Vec<String>>,
    /// Optional lifecycle statuses to match.
    pub statuses: Option<Vec<SkillStatus>>,
    /// Optional source formats to match across any version.
    pub formats: Option<Vec<SkillFormat>>,
    /// Optional skill schema versions to match across any version.
    pub schema_versions: Option<Vec<u32>>,
    /// Optional lower UTC timestamp bound for latest upload time.
    pub since: Option<DateTime<Utc>>,
    /// Optional upper UTC timestamp bound for latest upload time.
    pub until: Option<DateTime<Utc>>,
    /// Optional maximum number of returned summaries.
    pub limit: Option<usize>,
}

/// Machine-readable description of the skill-registry schema and searchable fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillRegistryManifest {
    /// Version of the persisted skill registry file.
    pub registry_version: u32,
    /// Current supported structured skill schema version.
    pub current_skill_schema_version: u32,
    /// Supported import and export formats.
    pub supported_formats: Vec<SkillFormat>,
    /// Searchable fields accepted by [`SkillQuery`].
    pub searchable_fields: Vec<String>,
    /// Required and optional parameters for `read_skill`.
    pub read_parameters: Vec<String>,
}

/// Report produced by a skill registry migration pass.
#[derive(Debug, Clone)]
pub struct SkillRegistryMigrationReport {
    /// Path of the skill registry file that was migrated.
    pub path: PathBuf,
    /// Number of skills whose versions were migrated.
    pub skills_migrated: usize,
    /// Total number of skill versions converted from V1 to V2 format.
    pub versions_migrated: usize,
    /// Source registry version.
    pub from_version: u32,
    /// Target registry version.
    pub to_version: u32,
}

// ---------------------------------------------------------------------------
// Persisted types (V2 current)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSkillRegistry {
    version: u32,
    skills: BTreeMap<String, SkillEntry>,
}

// ---------------------------------------------------------------------------
// V1 legacy shapes — used only for migration deserialization
// ---------------------------------------------------------------------------

/// Legacy V1 skill version shape — stores the full parsed document directly.
/// Used only for V1→V2 migration deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillVersionV1 {
    version_id: Uuid,
    uploaded_at: DateTime<Utc>,
    uploaded_by_agent_id: String,
    uploaded_by_agent_name: Option<String>,
    uploaded_by_agent_owner: Option<String>,
    source_format: SkillFormat,
    content_hash: String,
    document: SkillDocument,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillEntryV1 {
    skill_id: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    status: SkillStatus,
    status_reason: Option<String>,
    versions: Vec<SkillVersionV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSkillRegistryV1 {
    version: u32,
    skills: BTreeMap<String, SkillEntryV1>,
}

// ---------------------------------------------------------------------------
// In-memory index
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SkillIndexes {
    by_skill_id: HashMap<String, Vec<String>>,
    by_name: HashMap<String, Vec<String>>,
    by_tag: HashMap<String, Vec<String>>,
    by_trigger: HashMap<String, Vec<String>>,
    by_agent_id: HashMap<String, Vec<String>>,
    by_agent_name: HashMap<String, Vec<String>>,
    by_agent_owner: HashMap<String, Vec<String>>,
    by_status: HashMap<SkillStatus, Vec<String>>,
    by_format: HashMap<SkillFormat, Vec<String>>,
    by_schema_version: HashMap<u32, Vec<String>>,
}

impl SkillIndexes {
    fn from_entries(skills: &BTreeMap<String, SkillEntry>) -> Self {
        let mut indexes = Self::default();
        for (skill_id, entry) in skills {
            indexes.observe(skill_id, entry);
        }
        indexes
    }

    fn observe(&mut self, skill_id: &str, entry: &SkillEntry) {
        // Summarize may fail for corrupt entries; skip silently (integrity check runs on open).
        let Ok(summary) = summarize_entry(entry) else {
            return;
        };
        push_skill_index(
            &mut self.by_skill_id,
            skill_id.to_string(),
            skill_id.to_string(),
        );
        push_skill_index(
            &mut self.by_name,
            summary.name.to_lowercase(),
            skill_id.to_string(),
        );
        for tag in &summary.tags {
            push_skill_index(&mut self.by_tag, tag.to_lowercase(), skill_id.to_string());
        }
        for trigger in &summary.triggers {
            push_skill_index(
                &mut self.by_trigger,
                trigger.to_lowercase(),
                skill_id.to_string(),
            );
        }
        push_skill_index(&mut self.by_status, summary.status, skill_id.to_string());

        let mut agent_ids = HashSet::new();
        let mut agent_names = HashSet::new();
        let mut agent_owners = HashSet::new();
        let mut formats = HashSet::new();
        let mut schema_versions = HashSet::new();
        for (idx, version) in entry.versions.iter().enumerate() {
            agent_ids.insert(version.uploaded_by_agent_id.clone());
            if let Some(agent_name) = normalize_optional(version.uploaded_by_agent_name.as_deref())
            {
                agent_names.insert(agent_name);
            }
            if let Some(agent_owner) =
                normalize_optional(version.uploaded_by_agent_owner.as_deref())
            {
                agent_owners.insert(agent_owner);
            }
            formats.insert(version.source_format);
            // Reconstruct document to get schema_version for indexing.
            if let Ok(raw) = reconstruct_raw_content(entry, idx) {
                if let Ok(doc) = import_skill(&raw, version.source_format) {
                    schema_versions.insert(doc.schema_version);
                }
            }
        }
        for agent_id in agent_ids {
            push_skill_index(&mut self.by_agent_id, agent_id, skill_id.to_string());
        }
        for agent_name in agent_names {
            push_skill_index(
                &mut self.by_agent_name,
                agent_name.to_lowercase(),
                skill_id.to_string(),
            );
        }
        for agent_owner in agent_owners {
            push_skill_index(
                &mut self.by_agent_owner,
                agent_owner.to_lowercase(),
                skill_id.to_string(),
            );
        }
        for format in formats {
            push_skill_index(&mut self.by_format, format, skill_id.to_string());
        }
        for schema_version in schema_versions {
            push_skill_index(
                &mut self.by_schema_version,
                schema_version,
                skill_id.to_string(),
            );
        }
    }

    fn remove_skill(&mut self, skill_id: &str) {
        remove_skill_id_from_index(&mut self.by_skill_id, skill_id);
        remove_skill_id_from_index(&mut self.by_name, skill_id);
        remove_skill_id_from_index(&mut self.by_tag, skill_id);
        remove_skill_id_from_index(&mut self.by_trigger, skill_id);
        remove_skill_id_from_index(&mut self.by_agent_id, skill_id);
        remove_skill_id_from_index(&mut self.by_agent_name, skill_id);
        remove_skill_id_from_index(&mut self.by_agent_owner, skill_id);
        remove_skill_id_from_index(&mut self.by_status, skill_id);
        remove_skill_id_from_index(&mut self.by_format, skill_id);
        remove_skill_id_from_index(&mut self.by_schema_version, skill_id);
    }

    fn replace_skill(&mut self, skill_id: &str, entry: &SkillEntry) {
        self.remove_skill(skill_id);
        self.observe(skill_id, entry);
    }
}

// ---------------------------------------------------------------------------
// SkillRegistry
// ---------------------------------------------------------------------------

/// Durable, append-only skill registry backed by a versioned binary storage file.
///
/// ## Design: immutable versioned history
///
/// Every call to [`upload_skill`](SkillRegistry::upload_skill) creates a new
/// **immutable version** for the target skill id.  Versions are never
/// overwritten or deleted — the registry is append-only.  This gives you a
/// complete audit trail of how every skill has evolved over time.
///
/// To save storage space the first version of each skill is stored in full
/// ([`SkillVersionContent::Full`]); every subsequent version stores only a
/// compact unified-diff patch ([`SkillVersionContent::Delta`]) against the
/// immediately preceding version.  The registry reconstructs the full content
/// on demand by replaying patches forward from the base.
///
/// ## Lifecycle
///
/// Skills begin in the [`SkillStatus::Active`] state and can be transitioned to
/// [`SkillStatus::Deprecated`] (superseded, but still safe to use) or
/// [`SkillStatus::Revoked`] (must not be used) without losing any version
/// history.  Uploading a new version automatically restores `Active` status
/// unless the skill is currently `Revoked`.
///
/// ## Persistence
///
/// The registry is persisted as a single binary file named
/// `mentisdb-skills.bin` inside the MentisDB chain directory.  Writes use an
/// atomic rename so the file is never left in a partially-written state.
///
/// ## In-memory indexes
///
/// On open the registry builds in-memory indexes keyed by skill id, name, tag,
/// trigger phrase, agent id/name/owner, status, format, and schema version.
/// These indexes speed up [`search_skills`](SkillRegistry::search_skills) by
/// quickly narrowing the candidate set before the linear text filter runs.
///
/// # Examples
///
/// ```no_run
/// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat, SkillQuery, SkillStatus};
/// use std::path::PathBuf;
///
/// // ── Open (or create) the registry ──────────────────────────────────────
/// let dir = std::env::temp_dir().join("mentisdb_docs_example");
/// std::fs::create_dir_all(&dir).unwrap();
/// let mut registry = SkillRegistry::open(&dir).unwrap();
///
/// // ── Upload a skill ─────────────────────────────────────────────────────
/// let content = "# PR Reviewer\n\nReview pull requests for correctness and style.";
/// let upload = SkillUpload::new("agent-1", SkillFormat::Markdown, content)
///     .with_skill_id("pr-reviewer")
///     .with_agent_identity(Some("Reviewer Bot"), Some("acme"));
/// let summary = registry.upload_skill(upload).unwrap();
/// assert_eq!(summary.skill_id, "pr-reviewer");
/// assert_eq!(summary.version_count, 1);
///
/// // ── List all skills ────────────────────────────────────────────────────
/// let skills = registry.list_skills();
/// assert!(!skills.is_empty());
/// println!("Registry has {} skill(s)", skills.len());
///
/// // ── Search by tag ──────────────────────────────────────────────────────
/// let results = registry.search_skills(&SkillQuery {
///     statuses: Some(vec![SkillStatus::Active]),
///     ..Default::default()
/// });
/// assert!(!results.is_empty());
///
/// // ── Read back as Markdown ──────────────────────────────────────────────
/// let rendered = registry.read_skill("pr-reviewer", None, SkillFormat::Markdown).unwrap();
/// assert!(rendered.contains("PR Reviewer"));
/// ```
pub struct SkillRegistry {
    version: u32,
    skills: BTreeMap<String, SkillEntry>,
    storage_path: Option<PathBuf>,
    last_modified: Option<SystemTime>,
    indexes: SkillIndexes,
    latest_summaries: BTreeMap<String, SkillSummary>,
}

impl SkillRegistry {
    /// Open or create the skill registry stored under one MentisDB chain directory.
    ///
    /// The skill registry is independent from the thought-chain files but shares
    /// the same storage root so daemons and libraries can carry both durable
    /// memory and reusable skills together.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry file exists but cannot be decoded, if the
    /// registry version is too new, or if integrity verification fails.
    /// If the file is at V1, returns an error directing the caller to run
    /// [`migrate_skill_registry`] first.
    pub fn open<P: AsRef<Path>>(chain_dir: P) -> io::Result<Self> {
        let path = skill_registry_path(chain_dir.as_ref());
        Self::open_at_path(path)
    }

    /// Open or create the skill registry at an explicit binary file path.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` if the file is at V1 (migration required),
    /// too new, or fails integrity checks.
    pub fn open_at_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                version: MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
                skills: BTreeMap::new(),
                storage_path: Some(path),
                last_modified: None,
                indexes: SkillIndexes::default(),
                latest_summaries: BTreeMap::new(),
            });
        }

        let bytes = fs::read(&path)?;

        // Attempt to decode as current (V2) format first.
        let persisted: PersistedSkillRegistry =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map(|(registry, _)| registry)
                .map_err(|error| {
                    // If V2 decode fails, check whether it looks like a V1 registry.
                    if bincode::serde::decode_from_slice::<PersistedSkillRegistryV1, _>(
                        &bytes,
                        bincode::config::standard(),
                    )
                    .is_ok()
                    {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "skill registry is at V1; run migrate_skill_registry() before opening",
                        )
                    } else {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Failed to deserialize skill registry: {error}"),
                        )
                    }
                })?;

        if persisted.version == MENTISDB_SKILL_REGISTRY_V1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "skill registry is at V1; run migrate_skill_registry() before opening",
            ));
        }

        if persisted.version > MENTISDB_SKILL_REGISTRY_CURRENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported skill registry version {}", persisted.version),
            ));
        }

        verify_skill_registry_integrity(&persisted.skills)?;

        let last_modified = fs::metadata(&path).and_then(|m| m.modified()).ok();

        Ok(Self {
            version: persisted.version,
            indexes: SkillIndexes::from_entries(&persisted.skills),
            latest_summaries: build_latest_summaries(&persisted.skills)?,
            skills: persisted.skills,
            storage_path: Some(path),
            last_modified,
        })
    }

    /// Return the binary storage path used by this registry, if any.
    pub fn storage_path(&self) -> Option<&Path> {
        self.storage_path.as_deref()
    }

    /// Return the current registry manifest describing supported schema and search fields.
    pub fn manifest(&self) -> SkillRegistryManifest {
        SkillRegistryManifest {
            registry_version: self.version,
            current_skill_schema_version: MENTISDB_SKILL_CURRENT_SCHEMA_VERSION,
            supported_formats: vec![SkillFormat::Markdown, SkillFormat::Json],
            searchable_fields: vec![
                "text".to_string(),
                "skill_ids".to_string(),
                "names".to_string(),
                "tags_any".to_string(),
                "triggers_any".to_string(),
                "uploaded_by_agent_ids".to_string(),
                "uploaded_by_agent_names".to_string(),
                "uploaded_by_agent_owners".to_string(),
                "statuses".to_string(),
                "formats".to_string(),
                "schema_versions".to_string(),
                "since".to_string(),
                "until".to_string(),
                "limit".to_string(),
            ],
            read_parameters: vec![
                "skill_id".to_string(),
                "version_id".to_string(),
                "format".to_string(),
            ],
        }
    }

    /// Upload a skill file, parsing it through the requested import adapter.
    ///
    /// If `skill_id` is omitted from the [`SkillUpload`] request, the registry
    /// derives a URL-safe slug from the skill name found in the parsed content.
    /// Reusing an existing `skill_id` creates a new **immutable version** for
    /// that skill entry — all prior versions are preserved.
    ///
    /// ## Version storage
    ///
    /// - **Version 0**: stored as [`SkillVersionContent::Full`] (complete raw text).
    /// - **Version 1+**: stored as [`SkillVersionContent::Delta`] (unified diff
    ///   against the immediately preceding version).  This typically reduces
    ///   storage to the size of the change rather than the full document.
    ///
    /// ## Status behaviour
    ///
    /// Uploading a new version resets the status to [`SkillStatus::Active`]
    /// **unless** the skill is currently [`SkillStatus::Revoked`], in which case
    /// the revoked status is intentionally preserved to prevent accidentally
    /// re-activating a skill that was revoked for a safety reason.
    ///
    /// # Errors
    ///
    /// Returns an error if the request content cannot be parsed, validation fails,
    /// or the registry cannot be persisted.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_upload_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// let v1_content = "# Summariser\n\nSummarise long documents into bullet points.";
    /// let v1 = registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, v1_content)
    ///         .with_skill_id("summariser"),
    /// ).unwrap();
    /// assert_eq!(v1.version_count, 1); // first upload → version 0
    ///
    /// // A second upload to the same skill_id creates version 1 (stored as a delta).
    /// let v2_content = "# Summariser\n\nSummarise long documents into concise bullet points.";
    /// let v2 = registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, v2_content)
    ///         .with_skill_id("summariser"),
    /// ).unwrap();
    /// assert_eq!(v2.version_count, 2); // second upload → version 1
    ///
    /// // Confirm both versions are accessible.
    /// let versions = registry.skill_versions("summariser").unwrap();
    /// assert_eq!(versions.len(), 2);
    /// assert_eq!(versions[0].version_number, 0);
    /// assert_eq!(versions[1].version_number, 1);
    /// ```
    pub fn upload_skill(&mut self, request: SkillUpload<'_>) -> io::Result<SkillSummary> {
        let SkillUpload {
            skill_id,
            uploaded_by_agent_id,
            uploaded_by_agent_name,
            uploaded_by_agent_owner,
            format,
            content,
            signing_key_id,
            skill_signature,
        } = request;

        let document = import_skill(content, format)?;
        document.validate()?;
        let normalized_skill_id = skill_id
            .map(normalize_skill_id)
            .transpose()?
            .unwrap_or_else(|| derive_skill_id(&document.name));
        let now = Utc::now();

        // Resolve prev_raw before taking a mutable borrow on self.skills.
        let prev_raw: Option<String> = if let Some(existing) = self.skills.get(&normalized_skill_id)
        {
            let prev_index = existing.versions.len().saturating_sub(1);
            if existing.versions.is_empty() {
                None
            } else {
                Some(reconstruct_raw_content(existing, prev_index)?)
            }
        } else {
            None
        };

        let version_content = match &prev_raw {
            Some(prev) => {
                let patch = diffy::create_patch(prev.as_str(), content).to_string();
                SkillVersionContent::Delta { patch }
            }
            None => SkillVersionContent::Full {
                raw: content.to_string(),
            },
        };

        let content_hash = compute_content_hash(content);

        let entry = self
            .skills
            .entry(normalized_skill_id.clone())
            .or_insert_with(|| SkillEntry {
                skill_id: normalized_skill_id.clone(),
                created_at: now,
                updated_at: now,
                status: SkillStatus::Active,
                status_reason: None,
                versions: Vec::new(),
            });
        let version_number = entry.versions.len() as u32;
        entry.updated_at = now;
        if entry.status != SkillStatus::Revoked {
            entry.status = SkillStatus::Active;
            entry.status_reason = None;
        }
        entry.versions.push(SkillVersion {
            version_id: Uuid::new_v4(),
            version_number,
            uploaded_at: now,
            uploaded_by_agent_id: normalize_non_empty(
                uploaded_by_agent_id,
                "uploaded_by_agent_id",
            )?,
            uploaded_by_agent_name: normalize_optional(uploaded_by_agent_name),
            uploaded_by_agent_owner: normalize_optional(uploaded_by_agent_owner),
            source_format: format,
            content_hash,
            content: version_content,
            signing_key_id,
            skill_signature,
        });
        let summary = summarize_entry(entry)?;
        let entry = self
            .skills
            .get(&normalized_skill_id)
            .expect("uploaded skill entry must exist");
        self.indexes.replace_skill(&normalized_skill_id, entry);
        self.latest_summaries
            .insert(normalized_skill_id.clone(), summary.clone());
        self.persist()?;
        Ok(summary)
    }

    /// Return all stored skills as [`SkillSummary`] values ordered by most recent update first.
    ///
    /// Summaries carry the latest name, description, tags, triggers, warnings,
    /// current [`SkillStatus`], total version count, and uploader metadata.
    /// They do **not** contain the full skill text — use
    /// [`read_skill`](SkillRegistry::read_skill) to retrieve the rendered
    /// content.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat, SkillStatus};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_list_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, "# Alpha\n\nFirst skill.")
    ///         .with_skill_id("alpha"),
    /// ).unwrap();
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, "# Beta\n\nSecond skill.")
    ///         .with_skill_id("beta"),
    /// ).unwrap();
    ///
    /// let skills = registry.list_skills();
    /// // "beta" was uploaded last, so it appears first.
    /// assert_eq!(skills[0].skill_id, "beta");
    /// assert_eq!(skills[1].skill_id, "alpha");
    ///
    /// // Each summary exposes lifecycle status and version count.
    /// assert_eq!(skills[0].status, SkillStatus::Active);
    /// assert_eq!(skills[0].version_count, 1);
    /// ```
    pub fn list_skills(&self) -> Vec<SkillSummary> {
        let mut summaries: Vec<_> = self.latest_summaries.values().cloned().collect();
        summaries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        summaries
    }

    /// Search the skill registry using indexed filters plus optional text and time bounds.
    ///
    /// All filter fields in [`SkillQuery`] are optional.  An empty query
    /// (`SkillQuery::default()`) returns all skills, equivalent to
    /// [`list_skills`](SkillRegistry::list_skills).  Results are ordered by most
    /// recent update first and truncated to `query.limit` when set.
    ///
    /// ## How filtering works
    ///
    /// 1. **Indexed pass** — the registry resolves each non-`None` indexed field
    ///    (status, tags, agent id, format, …) using in-memory hash maps and
    ///    intersects the candidate sets.
    /// 2. **Linear pass** — the remaining candidates are checked against `since`,
    ///    `until`, and `text` filters in a single O(n) scan.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat, SkillQuery, SkillStatus};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_search_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// // Upload two skills with different tags.
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown,
    ///         "---\ntags: [memory, cache]\n---\n# Cache Manager\n\nManage in-memory caches.")
    ///         .with_skill_id("cache-manager"),
    /// ).unwrap();
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown,
    ///         "---\ntags: [io, network]\n---\n# HTTP Client\n\nMake HTTP requests.")
    ///         .with_skill_id("http-client"),
    /// ).unwrap();
    ///
    /// // Filter by tag — returns only the cache skill.
    /// let results = registry.search_skills(&SkillQuery {
    ///     tags_any: vec!["cache".into()],
    ///     ..Default::default()
    /// });
    /// assert_eq!(results.len(), 1);
    /// assert_eq!(results[0].skill_id, "cache-manager");
    ///
    /// // Filter by free text across name, description, and section bodies.
    /// let text_results = registry.search_skills(&SkillQuery {
    ///     text: Some("HTTP".into()),
    ///     statuses: Some(vec![SkillStatus::Active]),
    ///     ..Default::default()
    /// });
    /// assert_eq!(text_results[0].skill_id, "http-client");
    ///
    /// // Limit to 1 result.
    /// let limited = registry.search_skills(&SkillQuery {
    ///     limit: Some(1),
    ///     ..Default::default()
    /// });
    /// assert_eq!(limited.len(), 1);
    /// ```
    pub fn search_skills(&self, query: &SkillQuery) -> Vec<SkillSummary> {
        let candidate_ids = self.indexed_candidate_ids(query);
        let candidate_entries: Vec<&SkillEntry> = if let Some(ids) = candidate_ids {
            ids.into_iter()
                .filter_map(|skill_id| self.skills.get(&skill_id))
                .collect()
        } else {
            self.skills.values().collect()
        };

        let mut summaries: Vec<SkillSummary> = candidate_entries
            .into_iter()
            .filter_map(|entry| {
                let summary = self.latest_summaries.get(&entry.skill_id)?.clone();
                matches_skill_entry(entry, &summary, query).then_some(summary)
            })
            .collect();
        summaries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = query.limit {
            summaries.truncate(limit);
        }
        summaries
    }

    /// Return all immutable versions for one stored skill.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill does not exist.
    /// Returns `InvalidData` if any version\'s content cannot be reconstructed.
    pub fn skill_versions(&self, skill_id: &str) -> io::Result<Vec<SkillVersionSummary>> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        let mut summaries = Vec::with_capacity(entry.versions.len());
        for (idx, version) in entry.versions.iter().enumerate() {
            let raw = reconstruct_raw_content(entry, idx)?;
            let doc = import_skill(&raw, version.source_format)?;
            summaries.push(SkillVersionSummary {
                skill_id: entry.skill_id.clone(),
                version_id: version.version_id,
                version_number: version.version_number,
                uploaded_at: version.uploaded_at,
                uploaded_by_agent_id: version.uploaded_by_agent_id.clone(),
                uploaded_by_agent_name: version.uploaded_by_agent_name.clone(),
                uploaded_by_agent_owner: version.uploaded_by_agent_owner.clone(),
                source_format: version.source_format,
                schema_version: doc.schema_version,
                content_hash: version.content_hash.clone(),
                signing_key_id: version.signing_key_id.clone(),
            });
        }
        Ok(summaries)
    }

    /// Return the current summary for one stored skill.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill does not exist, or `InvalidData` if the
    /// latest version cannot be reconstructed.
    pub fn skill_summary(&self, skill_id: &str) -> io::Result<SkillSummary> {
        self.latest_summaries.get(skill_id).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })
    }

    pub(crate) fn cloned_entry(&self, skill_id: &str) -> io::Result<SkillEntry> {
        self.skills.get(skill_id).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })
    }

    /// Return one stored skill version, or the latest version when omitted.
    ///
    /// The returned [`SkillVersion`] carries the raw stored content (either
    /// [`SkillVersionContent::Full`] or [`SkillVersionContent::Delta`]). Use
    /// [`SkillRegistry::read_skill`] to obtain the rendered text.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill or version does not exist.
    pub fn skill_version(
        &self,
        skill_id: &str,
        version_id: Option<Uuid>,
    ) -> io::Result<SkillVersion> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        let version = match version_id {
            Some(version_id) => entry
                .versions
                .iter()
                .find(|version| version.version_id == version_id)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No version \'{version_id}\' found for skill \'{skill_id}\'"),
                    )
                })?,
            None => entry.latest_version(),
        };
        Ok(version.clone())
    }

    /// Reconstruct the full [`SkillDocument`] for a specific skill version, applying any
    /// delta patches from the base version forward.
    ///
    /// Use this method when you need access to the **structured object model** —
    /// sections, tags, triggers, warnings — rather than the rendered text.  For
    /// rendered text output (Markdown or JSON string) use
    /// [`read_skill`](SkillRegistry::read_skill).
    ///
    /// Pass `version_id: None` to retrieve the latest version.  Pass a specific
    /// [`Uuid`] (from [`SkillVersionSummary::version_id`]) to retrieve an older
    /// version.
    ///
    /// ## `skill_document` vs `read_skill`
    ///
    /// | Method           | Returns               | Use when …                              |
    /// |------------------|-----------------------|-----------------------------------------|
    /// | `skill_document` | `SkillDocument`       | You need structured fields (tags, …)    |
    /// | `read_skill`     | `String`              | You need rendered text for display/LLM  |
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill or version does not exist, or `InvalidData`
    /// if reconstruction or document parsing fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_document_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// let content = "---\ntags: [rust, safety]\n---\n# Borrow Checker\n\nExplain ownership rules.";
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, content)
    ///         .with_skill_id("borrow-checker"),
    /// ).unwrap();
    ///
    /// // Retrieve the structured document for the latest version.
    /// let doc = registry.skill_document("borrow-checker", None).unwrap();
    /// assert_eq!(doc.name, "Borrow Checker");
    /// assert!(doc.tags.contains(&"rust".to_string()));
    /// assert!(!doc.sections.is_empty());
    ///
    /// // The same content rendered as a string via read_skill.
    /// let markdown = registry.read_skill("borrow-checker", None, SkillFormat::Markdown).unwrap();
    /// assert!(markdown.contains("Borrow Checker"));
    /// ```
    pub fn skill_document(
        &self,
        skill_id: &str,
        version_id: Option<Uuid>,
    ) -> io::Result<SkillDocument> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        let version_index = match version_id {
            Some(vid) => entry
                .versions
                .iter()
                .position(|v| v.version_id == vid)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("No version \'{vid}\' found for skill \'{skill_id}\'"),
                    )
                })?,
            None => entry.versions.len().saturating_sub(1),
        };
        let version = &entry.versions[version_index];
        let raw = reconstruct_raw_content(entry, version_index)?;
        import_skill(&raw, version.source_format)
    }

    /// Read one stored skill rendered through the requested export adapter.
    ///
    /// The raw content is reconstructed by applying any stored delta patches,
    /// then parsed into a [`SkillDocument`] and re-exported in the requested
    /// `format`.  This means you can upload a skill as Markdown and read it
    /// back as JSON (or vice-versa) transparently.
    ///
    /// Pass `version_id: None` to retrieve the latest version.  Pass a specific
    /// [`Uuid`] (from [`SkillVersionSummary::version_id`]) to retrieve an older
    /// version.
    ///
    /// ## `read_skill` vs `skill_document`
    ///
    /// | Method           | Returns               | Use when …                              |
    /// |------------------|-----------------------|-----------------------------------------|
    /// | `read_skill`     | `String`              | You need rendered text for display/LLM  |
    /// | `skill_document` | `SkillDocument`       | You need structured fields (tags, …)    |
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill or version does not exist, or `InvalidData`
    /// if reconstruction, parsing, or re-export fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_read_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown,
    ///         "# Greeter\n\nGreet users warmly.")
    ///         .with_skill_id("greeter"),
    /// ).unwrap();
    ///
    /// // Read back as Markdown (same format as uploaded).
    /// let md = registry.read_skill("greeter", None, SkillFormat::Markdown).unwrap();
    /// assert!(md.contains("Greeter"));
    ///
    /// // Cross-format: read the same skill as JSON.
    /// let json = registry.read_skill("greeter", None, SkillFormat::Json).unwrap();
    /// assert!(json.contains("\"name\""));
    ///
    /// // Read a specific historical version by its version_id.
    /// let versions = registry.skill_versions("greeter").unwrap();
    /// let v0_id = versions[0].version_id;
    /// let v0 = registry.read_skill("greeter", Some(v0_id), SkillFormat::Markdown).unwrap();
    /// assert!(v0.contains("Greeter"));
    /// ```
    pub fn read_skill(
        &self,
        skill_id: &str,
        version_id: Option<Uuid>,
        format: SkillFormat,
    ) -> io::Result<String> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        Ok(read_skill_from_entry(skill_id, entry, version_id, format)?.content)
    }

    /// Mark one skill as deprecated while preserving all prior versions.
    ///
    /// `Deprecated` signals that the skill has been superseded — perhaps by a
    /// newer skill id or an updated version — but it is still **safe to read**
    /// and reference.  Callers and agents should surface the deprecation reason
    /// to users when available and prefer the successor skill.
    ///
    /// All version history is preserved; the change only updates the
    /// `status` field on the [`SkillEntry`].  A subsequent upload to the same
    /// `skill_id` will automatically restore [`SkillStatus::Active`].
    ///
    /// For a stronger signal (safety/correctness concern, must not be used) use
    /// [`revoke_skill`](SkillRegistry::revoke_skill) instead.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill does not exist.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat, SkillStatus};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_deprecate_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown, "# Old Summariser\n\nLegacy approach.")
    ///         .with_skill_id("old-summariser"),
    /// ).unwrap();
    ///
    /// // Mark the skill as deprecated with an explanatory reason.
    /// let summary = registry.deprecate_skill(
    ///     "old-summariser",
    ///     Some("Superseded by 'summariser-v2'; migrate all usages."),
    /// ).unwrap();
    ///
    /// assert_eq!(summary.status, SkillStatus::Deprecated);
    /// assert!(summary.status_reason.as_deref().unwrap().contains("Superseded"));
    ///
    /// // All version history is preserved — version_count is unchanged.
    /// assert_eq!(summary.version_count, 1);
    ///
    /// // The skill is still visible in list/search results.
    /// let skills = registry.list_skills();
    /// assert!(skills.iter().any(|s| s.skill_id == "old-summariser"));
    /// ```
    pub fn deprecate_skill(
        &mut self,
        skill_id: &str,
        reason: Option<&str>,
    ) -> io::Result<SkillSummary> {
        let entry = self.skills.get_mut(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        entry.status = SkillStatus::Deprecated;
        entry.status_reason = normalize_optional(reason);
        entry.updated_at = Utc::now();
        let summary = summarize_entry(entry)?;
        let entry = self
            .skills
            .get(skill_id)
            .expect("deprecated skill entry must exist");
        self.indexes.replace_skill(skill_id, entry);
        self.latest_summaries
            .insert(skill_id.to_string(), summary.clone());
        self.persist()?;
        Ok(summary)
    }

    /// Mark one skill as revoked while preserving all prior versions for auditability.
    ///
    /// `Revoked` is the strongest lifecycle signal: it indicates a **safety or
    /// correctness concern** that makes the skill unsuitable for use.  Agents
    /// and callers must treat revoked skills as untrusted and must not execute
    /// or apply them.
    ///
    /// Unlike [`deprecate_skill`](SkillRegistry::deprecate_skill), a revoked
    /// skill's status is **not** automatically cleared by a subsequent upload to
    /// the same `skill_id`.  This is intentional: publish a corrected skill under
    /// a new `skill_id` and leave the revoked entry as a permanent audit record.
    ///
    /// All version history is preserved; the change only updates the `status`
    /// field on the [`SkillEntry`].
    ///
    /// # Errors
    ///
    /// Returns `NotFound` if the skill does not exist.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mentisdb::{SkillRegistry, SkillUpload, SkillFormat, SkillStatus};
    ///
    /// let dir = std::env::temp_dir().join("mentisdb_revoke_example");
    /// std::fs::create_dir_all(&dir).unwrap();
    /// let mut registry = SkillRegistry::open(&dir).unwrap();
    ///
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown,
    ///         "# Dangerous Op\n\nDo something risky.")
    ///         .with_skill_id("dangerous-op"),
    /// ).unwrap();
    ///
    /// // Revoke the skill with a safety explanation.
    /// let summary = registry.revoke_skill(
    ///     "dangerous-op",
    ///     Some("CVE-2024-0001: skill can execute arbitrary code; do not use."),
    /// ).unwrap();
    ///
    /// assert_eq!(summary.status, SkillStatus::Revoked);
    /// assert!(summary.status_reason.as_deref().unwrap().contains("CVE"));
    ///
    /// // Version history is preserved — the skill is still readable for audit.
    /// assert_eq!(summary.version_count, 1);
    ///
    /// // Uploading a new version does NOT clear the Revoked status.
    /// registry.upload_skill(
    ///     SkillUpload::new("agent-1", SkillFormat::Markdown,
    ///         "# Dangerous Op\n\nUpdated content (still revoked).")
    ///         .with_skill_id("dangerous-op"),
    /// ).unwrap();
    /// let after = registry.skill_summary("dangerous-op").unwrap();
    /// assert_eq!(after.status, SkillStatus::Revoked); // still revoked!
    /// ```
    pub fn revoke_skill(
        &mut self,
        skill_id: &str,
        reason: Option<&str>,
    ) -> io::Result<SkillSummary> {
        let entry = self.skills.get_mut(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill \'{skill_id}\' found"),
            )
        })?;
        entry.status = SkillStatus::Revoked;
        entry.status_reason = normalize_optional(reason);
        entry.updated_at = Utc::now();
        let summary = summarize_entry(entry)?;
        let entry = self
            .skills
            .get(skill_id)
            .expect("revoked skill entry must exist");
        self.indexes.replace_skill(skill_id, entry);
        self.latest_summaries
            .insert(skill_id.to_string(), summary.clone());
        self.persist()?;
        Ok(summary)
    }

    fn indexed_candidate_ids(&self, query: &SkillQuery) -> Option<Vec<String>> {
        let mut filters = Vec::new();

        if let Some(skill_ids) = &query.skill_ids {
            filters.push(union_skill_id_lists(
                skill_ids
                    .iter()
                    .filter_map(|skill_id| self.indexes.by_skill_id.get(skill_id)),
            ));
        }
        if let Some(names) = &query.names {
            filters.push(union_skill_id_lists(
                names
                    .iter()
                    .filter_map(|name| self.indexes.by_name.get(&name.to_lowercase())),
            ));
        }
        if !query.tags_any.is_empty() {
            filters.push(union_skill_id_lists(
                query
                    .tags_any
                    .iter()
                    .filter_map(|tag| self.indexes.by_tag.get(&tag.to_lowercase())),
            ));
        }
        if !query.triggers_any.is_empty() {
            filters.push(union_skill_id_lists(query.triggers_any.iter().filter_map(
                |trigger| self.indexes.by_trigger.get(&trigger.to_lowercase()),
            )));
        }
        if let Some(agent_ids) = &query.uploaded_by_agent_ids {
            filters.push(union_skill_id_lists(
                agent_ids
                    .iter()
                    .filter_map(|agent_id| self.indexes.by_agent_id.get(agent_id)),
            ));
        }
        if let Some(agent_names) = &query.uploaded_by_agent_names {
            filters.push(union_skill_id_lists(agent_names.iter().filter_map(
                |agent_name| self.indexes.by_agent_name.get(&agent_name.to_lowercase()),
            )));
        }
        if let Some(agent_owners) = &query.uploaded_by_agent_owners {
            filters.push(union_skill_id_lists(agent_owners.iter().filter_map(
                |agent_owner| self.indexes.by_agent_owner.get(&agent_owner.to_lowercase()),
            )));
        }
        if let Some(statuses) = &query.statuses {
            filters.push(union_skill_id_lists(
                statuses
                    .iter()
                    .filter_map(|status| self.indexes.by_status.get(status)),
            ));
        }
        if let Some(formats) = &query.formats {
            filters.push(union_skill_id_lists(
                formats
                    .iter()
                    .filter_map(|format| self.indexes.by_format.get(format)),
            ));
        }
        if let Some(schema_versions) = &query.schema_versions {
            filters.push(union_skill_id_lists(
                schema_versions
                    .iter()
                    .filter_map(|version| self.indexes.by_schema_version.get(version)),
            ));
        }

        let mut filters = filters.into_iter();
        let first = filters.next()?;
        Some(filters.fold(first, |acc, values| intersect_skill_ids(&acc, &values)))
    }

    fn persist(&mut self) -> io::Result<()> {
        let Some(path) = &self.storage_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let payload = bincode::serde::encode_to_vec(
            PersistedSkillRegistry {
                version: self.version,
                skills: self.skills.clone(),
            },
            bincode::config::standard(),
        )
        .map_err(|error| {
            io::Error::other(format!("Failed to serialize skill registry: {error}"))
        })?;
        let temp_path = path.with_extension("bin.tmp");
        fs::write(&temp_path, payload)?;
        fs::rename(&temp_path, path)?;
        self.last_modified = fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(())
    }

    /// Reload the registry from disk when the backing file has changed.
    ///
    /// Returns `Ok(true)` when a reload occurred, `Ok(false)` when the
    /// in-memory registry was already up to date, and propagates I/O errors for
    /// unexpected disk failures.
    pub fn refresh_from_disk_if_stale(&mut self) -> io::Result<bool> {
        let Some(path) = self.storage_path.clone() else {
            return Ok(false);
        };

        let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
        if modified.is_some() && self.last_modified.as_ref() == modified.as_ref() {
            return Ok(false);
        }
        if modified.is_none() && self.last_modified.is_none() {
            return Ok(false);
        }

        let fresh = SkillRegistry::open_at_path(&path)?;
        *self = fresh;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------

/// Migrates the skill registry file at `chain_dir` from V1 to the current V2 format, if needed.
///
/// - If the file does not exist or is already at the current version, returns `Ok(None)`.
/// - If the file is at V1 (full `SkillDocument` per version), each version is converted to
///   [`SkillVersionContent::Full`] by re-exporting the document to its source format.
///   All versions are stored as `Full` (not delta) during migration; delta storage applies
///   only to uploads made after migration.
/// - Saves the migrated registry in place and returns a [`SkillRegistryMigrationReport`].
///
/// This function is idempotent: running it on an already-migrated registry is a no-op.
///
/// # Errors
///
/// Returns an error if the file cannot be read, decoded, encoded, or written.
pub fn migrate_skill_registry<P: AsRef<Path>>(
    chain_dir: P,
) -> io::Result<Option<SkillRegistryMigrationReport>> {
    let path = skill_registry_path(chain_dir.as_ref());
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;

    // Try current version first — if it loads cleanly, no migration needed.
    if let Ok((persisted, _)) = bincode::serde::decode_from_slice::<PersistedSkillRegistry, _>(
        &bytes,
        bincode::config::standard(),
    ) {
        if persisted.version >= MENTISDB_SKILL_REGISTRY_CURRENT_VERSION {
            return Ok(None);
        }
    }

    // Fall back to V1 shape.
    let (v1, _): (PersistedSkillRegistryV1, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("skill registry is neither V1 nor current version: {e}"),
            )
        })?;

    let mut skills_migrated = 0usize;
    let mut versions_migrated = 0usize;
    let mut migrated_skills: BTreeMap<String, SkillEntry> = BTreeMap::new();

    for (skill_id, entry_v1) in &v1.skills {
        let mut versions_v2: Vec<SkillVersion> = Vec::with_capacity(entry_v1.versions.len());
        for (idx, ver_v1) in entry_v1.versions.iter().enumerate() {
            let raw = export_skill(&ver_v1.document, ver_v1.source_format)?;
            let content_hash = compute_content_hash(&raw);
            versions_v2.push(SkillVersion {
                version_id: ver_v1.version_id,
                version_number: idx as u32,
                uploaded_at: ver_v1.uploaded_at,
                uploaded_by_agent_id: ver_v1.uploaded_by_agent_id.clone(),
                uploaded_by_agent_name: ver_v1.uploaded_by_agent_name.clone(),
                uploaded_by_agent_owner: ver_v1.uploaded_by_agent_owner.clone(),
                source_format: ver_v1.source_format,
                content_hash,
                content: SkillVersionContent::Full { raw },
                signing_key_id: None,
                skill_signature: None,
            });
            versions_migrated += 1;
        }
        migrated_skills.insert(
            skill_id.clone(),
            SkillEntry {
                skill_id: skill_id.clone(),
                created_at: entry_v1.created_at,
                updated_at: entry_v1.updated_at,
                status: entry_v1.status,
                status_reason: entry_v1.status_reason.clone(),
                versions: versions_v2,
            },
        );
        skills_migrated += 1;
    }

    let new_persisted = PersistedSkillRegistry {
        version: MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
        skills: migrated_skills,
    };
    let encoded = bincode::serde::encode_to_vec(&new_persisted, bincode::config::standard())
        .map_err(|e| io::Error::other(format!("encode error: {e}")))?;
    fs::write(&path, &encoded)?;
    Ok(Some(SkillRegistryMigrationReport {
        path,
        skills_migrated,
        versions_migrated,
        from_version: v1.version,
        to_version: MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
    }))
}

// ---------------------------------------------------------------------------
// Public skill import/export adapters
// ---------------------------------------------------------------------------

/// Import a skill file through the requested adapter into the structured [`SkillDocument`] model.
///
/// This is the parsing entry-point for both Markdown and JSON skill source.
/// For Markdown, the parser recognises an optional `---` … `---` YAML-like
/// frontmatter block followed by heading-delimited sections.  For JSON, the
/// content is deserialized directly into a [`SkillDocument`].
///
/// Use [`export_skill`] to render a `SkillDocument` back to either format,
/// completing the round-trip.
///
/// # Errors
///
/// Returns `InvalidData` if the content cannot be parsed.
///
/// # Examples
///
/// ```rust
/// use mentisdb::{import_skill, export_skill, SkillFormat};
///
/// // Markdown with frontmatter; section headings use ## so they are not
/// // stripped by rustdoc's hidden-line logic (lines starting with `# `).
/// let markdown = r#"---
/// name: Code Style Guide
/// description: Enforce consistent code style.
/// tags: [rust, style]
/// ---
///
/// ## Rules
///
/// - Use 4-space indentation.
/// - Prefer `match` over long `if-else` chains.
/// "#;
///
/// // Parse Markdown → SkillDocument.
/// let doc = import_skill(markdown, SkillFormat::Markdown).unwrap();
/// assert_eq!(doc.name, "Code Style Guide");
/// assert!(doc.tags.contains(&"rust".to_string()));
/// assert_eq!(doc.sections[0].heading, "Rules");
///
/// // Round-trip: export to JSON then re-import — document is identical.
/// let json = export_skill(&doc, SkillFormat::Json).unwrap();
/// let doc2 = import_skill(&json, SkillFormat::Json).unwrap();
/// assert_eq!(doc, doc2);
///
/// // Cross-format: export back to Markdown from JSON.
/// let md_again = export_skill(&doc2, SkillFormat::Markdown).unwrap();
/// assert!(md_again.contains("Code Style Guide"));
/// ```
pub fn import_skill(content: &str, format: SkillFormat) -> io::Result<SkillDocument> {
    match format {
        SkillFormat::Markdown => parse_markdown_skill(content),
        SkillFormat::Json => serde_json::from_str(content).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse skill JSON: {error}"),
            )
        }),
    }
}

/// Export a structured [`SkillDocument`] through the requested adapter.
///
/// Renders the `SkillDocument` to a raw string in the target `format`.  The
/// Markdown renderer emits a frontmatter block followed by the section
/// headings and bodies.  The JSON renderer uses `serde_json::to_string_pretty`.
///
/// Use [`import_skill`] to parse raw text back into a `SkillDocument`,
/// completing the round-trip.
///
/// # Errors
///
/// Returns `InvalidInput` if the document fails validation (e.g. empty name
/// or description), or an `io::Error` if JSON serialization fails.
///
/// # Examples
///
/// ```rust
/// use mentisdb::{import_skill, export_skill, SkillFormat, SkillDocument, SkillSection};
/// use mentisdb::MENTISDB_SKILL_CURRENT_SCHEMA_VERSION;
///
/// // Build a document programmatically and export it.
/// let doc = SkillDocument {
///     schema_version: MENTISDB_SKILL_CURRENT_SCHEMA_VERSION,
///     name: "Greeter".into(),
///     description: "Greet users warmly.".into(),
///     tags: vec!["greeting".into()],
///     triggers: vec!["say hello".into()],
///     warnings: vec![],
///     sections: vec![
///         SkillSection {
///             level: 1,
///             heading: "Greeter".into(),
///             body: "Greet users warmly.".into(),
///         },
///         SkillSection {
///             level: 2,
///             heading: "Examples".into(),
///             body: "Hello, world!".into(),
///         },
///     ],
/// };
///
/// // Export to Markdown.
/// let md = export_skill(&doc, SkillFormat::Markdown).unwrap();
/// assert!(md.starts_with("---\n"));
/// assert!(md.contains("# Greeter"));
///
/// // Export to JSON.
/// let json = export_skill(&doc, SkillFormat::Json).unwrap();
/// assert!(json.contains("\"name\": \"Greeter\""));
///
/// // Round-trip through Markdown: re-import gives an equivalent document.
/// let doc2 = import_skill(&md, SkillFormat::Markdown).unwrap();
/// assert_eq!(doc.name, doc2.name);
/// assert_eq!(doc.tags, doc2.tags);
/// ```
pub fn export_skill(skill: &SkillDocument, format: SkillFormat) -> io::Result<String> {
    skill.validate()?;
    match format {
        SkillFormat::Markdown => Ok(render_markdown_skill(skill)),
        SkillFormat::Json => serde_json::to_string_pretty(skill)
            .map_err(|error| io::Error::other(format!("Failed to serialize skill JSON: {error}"))),
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Reconstructs the full raw content string for the skill version at `version_index`
/// by applying delta patches from the base version forward.
///
/// The base version (index 0) must be stored as [`SkillVersionContent::Full`].
/// Intermediate versions may be either `Full` or `Delta`.
fn reconstruct_raw_content(entry: &SkillEntry, version_index: usize) -> io::Result<String> {
    if entry.versions.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "skill has no versions",
        ));
    }
    // Base must be Full
    let base = match &entry.versions[0].content {
        SkillVersionContent::Full { raw } => raw.clone(),
        SkillVersionContent::Delta { .. } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "version 0 must be Full content",
            ))
        }
    };
    let mut current = base;
    for i in 1..=version_index {
        match &entry.versions[i].content {
            SkillVersionContent::Full { raw } => {
                current = raw.clone();
            }
            SkillVersionContent::Delta { patch } => {
                let parsed_patch = diffy::Patch::from_str(patch).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid patch at v{i}: {e}"),
                    )
                })?;
                current = diffy::apply(&current, &parsed_patch).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("failed to apply patch at v{i}: {e}"),
                    )
                })?;
            }
        }
    }
    Ok(current)
}

pub(crate) struct SkillReadSnapshot {
    pub version: SkillVersion,
    pub content: String,
    pub schema_version: u32,
}

pub(crate) fn read_skill_from_entry(
    skill_id: &str,
    entry: &SkillEntry,
    version_id: Option<Uuid>,
    format: SkillFormat,
) -> io::Result<SkillReadSnapshot> {
    let version_index = match version_id {
        Some(vid) => entry
            .versions
            .iter()
            .position(|v| v.version_id == vid)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No version \'{vid}\' found for skill \'{skill_id}'"),
                )
            })?,
        None => entry.versions.len().saturating_sub(1),
    };
    let version = entry.versions[version_index].clone();
    let raw = reconstruct_raw_content(entry, version_index)?;
    let document = import_skill(&raw, version.source_format)?;
    Ok(SkillReadSnapshot {
        version,
        content: export_skill(&document, format)?,
        schema_version: document.schema_version,
    })
}

/// Computes the SHA-256 hex digest of the given raw skill content string.
fn compute_content_hash(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn parse_markdown_skill(content: &str) -> io::Result<SkillDocument> {
    let mut schema_version = MENTISDB_SKILL_CURRENT_SCHEMA_VERSION;
    let mut frontmatter_name = None;
    let mut frontmatter_description = None;
    let mut tags = Vec::new();
    let mut triggers = Vec::new();
    let mut warnings = Vec::new();
    let mut body = content;

    if let Some(stripped) = body.strip_prefix("---\n") {
        if let Some((frontmatter, remainder)) = stripped.split_once("\n---\n") {
            body = remainder;
            for line in frontmatter.lines() {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                match key {
                    "schema_version" => {
                        schema_version = value.parse::<u32>().map_err(|error| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("Invalid skill schema_version \'{value}\': {error}"),
                            )
                        })?;
                    }
                    "name" => frontmatter_name = Some(trim_wrapped(value).to_string()),
                    "description" => {
                        frontmatter_description = Some(trim_wrapped(value).to_string())
                    }
                    "tags" => tags = parse_frontmatter_list(value),
                    "triggers" => triggers = parse_frontmatter_list(value),
                    "warnings" => warnings = parse_frontmatter_list(value),
                    _ => {}
                }
            }
        }
    }

    let mut sections = Vec::new();
    let mut current_heading = None;
    let mut current_level = 0_u8;
    let mut current_body = Vec::new();
    let mut intro = Vec::new();

    for line in body.lines() {
        if let Some((level, heading)) = parse_heading(line) {
            if let Some(existing_heading) = current_heading.take() {
                sections.push(SkillSection {
                    level: current_level,
                    heading: existing_heading,
                    body: current_body.join("\n").trim().to_string(),
                });
                current_body.clear();
            }
            current_level = level;
            current_heading = Some(heading);
        } else if current_heading.is_some() {
            current_body.push(line.to_string());
        } else {
            intro.push(line.to_string());
        }
    }

    if let Some(existing_heading) = current_heading {
        sections.push(SkillSection {
            level: current_level,
            heading: existing_heading,
            body: current_body.join("\n").trim().to_string(),
        });
    }

    if sections.is_empty() && !body.trim().is_empty() {
        sections.push(SkillSection {
            level: 1,
            heading: "Instructions".to_string(),
            body: body.trim().to_string(),
        });
    }

    let name = frontmatter_name
        .or_else(|| {
            sections
                .iter()
                .find(|section| section.level == 1)
                .map(|section| section.heading.clone())
        })
        .unwrap_or_else(|| "unnamed-skill".to_string());
    let description = frontmatter_description
        .unwrap_or_else(|| intro.join("\n").trim().to_string())
        .trim()
        .to_string();

    Ok(SkillDocument {
        schema_version,
        name,
        description,
        tags: normalize_list(tags),
        triggers: normalize_list(triggers),
        warnings: normalize_list(warnings),
        sections,
    })
}

fn render_markdown_skill(skill: &SkillDocument) -> String {
    let mut markdown = String::new();
    markdown.push_str("---\n");
    markdown.push_str(&format!("schema_version: {}\n", skill.schema_version));
    markdown.push_str(&format!("name: {}\n", skill.name));
    markdown.push_str(&format!("description: {}\n", skill.description));
    if !skill.tags.is_empty() {
        markdown.push_str(&format!("tags: [{}]\n", skill.tags.join(", ")));
    }
    if !skill.triggers.is_empty() {
        markdown.push_str(&format!("triggers: [{}]\n", skill.triggers.join(", ")));
    }
    if !skill.warnings.is_empty() {
        markdown.push_str(&format!("warnings: [{}]\n", skill.warnings.join(", ")));
    }
    markdown.push_str("---\n\n");
    markdown.push_str(&format!("# {}\n\n", skill.name));
    markdown.push_str(&format!("{}\n", skill.description.trim()));
    for section in &skill.sections {
        let heading_marks = "#".repeat(section.level.clamp(1, 6) as usize);
        markdown.push_str(&format!("\n{} {}\n\n", heading_marks, section.heading));
        if !section.body.trim().is_empty() {
            markdown.push_str(section.body.trim());
            markdown.push('\n');
        }
    }
    markdown
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let level = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let heading = trimmed[level..].trim();
    if heading.is_empty() {
        return None;
    }
    Some((level as u8, heading.to_string()))
}

fn parse_frontmatter_list(value: &str) -> Vec<String> {
    let trimmed = trim_wrapped(value).trim();
    let trimmed = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);
    trimmed
        .split(',')
        .map(trim_wrapped)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn trim_wrapped(value: &str) -> &str {
    value.trim().trim_matches('"').trim_matches('\'')
}

fn normalize_list(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_lowercase();
        if seen.insert(key) {
            normalized.push(trimmed.to_string());
        }
    }
    normalized
}

fn normalize_non_empty(value: &str, field_name: &str) -> io::Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{field_name} must not be empty"),
        ));
    }
    Ok(normalized.to_string())
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn derive_skill_id(name: &str) -> String {
    let slug = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let mut normalized = slug
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if normalized.is_empty() {
        normalized = "skill".to_string();
    }
    normalized
}

fn normalize_skill_id(value: &str) -> io::Result<String> {
    let normalized = derive_skill_id(value);
    if normalized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "skill_id must not be empty",
        ));
    }
    Ok(normalized)
}

/// Build a [`SkillSummary`] from a [`SkillEntry`] by reconstructing the latest
/// version\'s raw content and parsing the document.
///
/// # Errors
///
/// Returns `InvalidData` if the latest version cannot be reconstructed or parsed.
fn summarize_entry(entry: &SkillEntry) -> io::Result<SkillSummary> {
    let latest = entry.latest_version();
    let latest_index = entry.versions.len() - 1;
    let raw = reconstruct_raw_content(entry, latest_index)?;
    let doc = import_skill(&raw, latest.source_format)?;
    Ok(SkillSummary {
        skill_id: entry.skill_id.clone(),
        name: doc.name,
        description: doc.description,
        status: entry.status,
        status_reason: entry.status_reason.clone(),
        schema_version: doc.schema_version,
        tags: doc.tags,
        triggers: doc.triggers,
        warnings: doc.warnings,
        latest_version_id: latest.version_id,
        version_count: entry.versions.len(),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        latest_uploaded_at: latest.uploaded_at,
        latest_uploaded_by_agent_id: latest.uploaded_by_agent_id.clone(),
        latest_uploaded_by_agent_name: latest.uploaded_by_agent_name.clone(),
        latest_uploaded_by_agent_owner: latest.uploaded_by_agent_owner.clone(),
        latest_source_format: latest.source_format,
    })
}

fn matches_skill_entry(entry: &SkillEntry, summary: &SkillSummary, query: &SkillQuery) -> bool {
    if let Some(since) = query.since {
        if summary.latest_uploaded_at < since {
            return false;
        }
    }
    if let Some(until) = query.until {
        if summary.latest_uploaded_at > until {
            return false;
        }
    }
    if let Some(text) = &query.text {
        let needle = text.to_lowercase();
        let mut haystacks = vec![
            summary.name.to_lowercase(),
            summary.description.to_lowercase(),
        ];
        haystacks.extend(
            summary
                .warnings
                .iter()
                .map(|warning| warning.to_lowercase()),
        );
        // Reconstruct the latest version document for section-body text search.
        let latest_index = entry.versions.len().saturating_sub(1);
        if let Ok(raw) = reconstruct_raw_content(entry, latest_index) {
            let format = entry.latest_version().source_format;
            if let Ok(doc) = import_skill(&raw, format) {
                haystacks.extend(doc.sections.iter().map(|s| s.heading.to_lowercase()));
                haystacks.extend(doc.sections.iter().map(|s| s.body.to_lowercase()));
            }
        }
        if !haystacks.iter().any(|value| value.contains(&needle)) {
            return false;
        }
    }
    true
}

fn push_skill_index<K: Eq + std::hash::Hash>(
    index: &mut HashMap<K, Vec<String>>,
    key: K,
    skill_id: String,
) {
    let values = index.entry(key).or_default();
    if !values.iter().any(|existing| existing == &skill_id) {
        values.push(skill_id);
    }
}

fn remove_skill_id_from_index<K: Eq + std::hash::Hash>(
    index: &mut HashMap<K, Vec<String>>,
    skill_id: &str,
) {
    index.retain(|_, values| {
        values.retain(|existing| existing != skill_id);
        !values.is_empty()
    });
}

fn build_latest_summaries(
    skills: &BTreeMap<String, SkillEntry>,
) -> io::Result<BTreeMap<String, SkillSummary>> {
    let mut summaries = BTreeMap::new();
    for (skill_id, entry) in skills {
        summaries.insert(skill_id.clone(), summarize_entry(entry)?);
    }
    Ok(summaries)
}

fn union_skill_id_lists<'a, I>(lists: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a Vec<String>>,
{
    let mut skill_ids: Vec<String> = lists
        .into_iter()
        .flat_map(|values| values.iter().cloned())
        .collect();
    skill_ids.sort();
    skill_ids.dedup();
    skill_ids
}

fn intersect_skill_ids(left: &[String], right: &[String]) -> Vec<String> {
    let left_set: HashSet<&String> = left.iter().collect();
    let mut result: Vec<String> = right
        .iter()
        .filter(|value| left_set.contains(value))
        .cloned()
        .collect();
    result.sort();
    result.dedup();
    result
}

fn verify_skill_registry_integrity(skills: &BTreeMap<String, SkillEntry>) -> io::Result<()> {
    for (skill_id, entry) in skills {
        if entry.versions.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Skill \'{skill_id}\' has no versions"),
            ));
        }
        for (idx, version) in entry.versions.iter().enumerate() {
            let raw = reconstruct_raw_content(entry, idx)?;
            let expected = compute_content_hash(&raw);
            if version.content_hash != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Skill version \'{}\' for skill \'{}\' failed integrity verification",
                        version.version_id, skill_id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn skill_registry_path(chain_dir: &Path) -> PathBuf {
    chain_dir.join(MENTISDB_SKILL_REGISTRY_FILENAME)
}
