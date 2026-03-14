use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

/// First supported version of the persisted skill registry file.
pub const MENTISDB_SKILL_REGISTRY_V1: u32 = 1;
/// Alias for the current persisted skill registry file version.
pub const MENTISDB_SKILL_REGISTRY_CURRENT_VERSION: u32 = MENTISDB_SKILL_REGISTRY_V1;
/// First supported version of the structured skill schema.
pub const MENTISDB_SKILL_SCHEMA_V1: u32 = 1;
/// Alias for the current structured skill schema version.
pub const MENTISDB_SKILL_CURRENT_SCHEMA_VERSION: u32 = MENTISDB_SKILL_SCHEMA_V1;
const MENTISDB_SKILL_REGISTRY_FILENAME: &str = "mentisdb-skills.bin";

/// Supported import and export formats for skill documents.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillFormat {
    /// Markdown skill document with optional YAML-like frontmatter.
    Markdown,
    /// JSON representation of the structured skill document.
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
                "Unsupported skill format '{other}'. Expected 'markdown' or 'json'"
            )),
        }
    }
}

/// Lifecycle state of a stored skill entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SkillStatus {
    /// The skill is active and should be returned normally.
    Active,
    /// The skill is superseded but still readable.
    Deprecated,
    /// The skill should not be trusted for normal use.
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
                "Unsupported skill status '{other}'. Expected 'active', 'deprecated', or 'revoked'"
            )),
        }
    }
}

/// One heading-delimited section of a structured skill document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillSection {
    /// Markdown heading level, from `1` (`#`) through `6` (`######`).
    pub level: u8,
    /// Section heading text without the leading `#` markers.
    pub heading: String,
    /// Section body text.
    pub body: String,
}

/// Structured representation of a skill file.
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

/// One immutable uploaded skill version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillVersion {
    /// Stable unique version identifier.
    pub version_id: Uuid,
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
    /// SHA-256 hash of the canonical structured version payload.
    pub content_hash: String,
    /// Structured skill content.
    pub document: SkillDocument,
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
}

/// Query parameters for skill-registry search.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSkillRegistry {
    version: u32,
    skills: BTreeMap<String, SkillEntry>,
}

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
        let summary = summarize_entry(entry);
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
        for version in &entry.versions {
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
            schema_versions.insert(version.document.schema_version);
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
}

/// Durable skill registry backed by a versioned binary storage file.
pub struct SkillRegistry {
    version: u32,
    skills: BTreeMap<String, SkillEntry>,
    storage_path: Option<PathBuf>,
    indexes: SkillIndexes,
}

impl SkillRegistry {
    /// Open or create the skill registry stored under one MentisDB chain directory.
    ///
    /// The skill registry is independent from the thought-chain files but shares
    /// the same storage root so daemons and libraries can carry both durable
    /// memory and reusable skills together.
    pub fn open<P: AsRef<Path>>(chain_dir: P) -> io::Result<Self> {
        let path = skill_registry_path(chain_dir.as_ref());
        Self::open_at_path(path)
    }

    /// Open or create the skill registry at an explicit binary file path.
    pub fn open_at_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                version: MENTISDB_SKILL_REGISTRY_CURRENT_VERSION,
                skills: BTreeMap::new(),
                storage_path: Some(path),
                indexes: SkillIndexes::default(),
            });
        }

        let bytes = fs::read(&path)?;
        let persisted: PersistedSkillRegistry =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map(|(registry, _)| registry)
                .map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to deserialize skill registry: {error}"),
                    )
                })?;

        if persisted.version > MENTISDB_SKILL_REGISTRY_CURRENT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported skill registry version {}", persisted.version),
            ));
        }

        verify_skill_registry_integrity(&persisted.skills)?;

        Ok(Self {
            version: persisted.version,
            indexes: SkillIndexes::from_entries(&persisted.skills),
            skills: persisted.skills,
            storage_path: Some(path),
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
    /// If `skill_id` is omitted, the registry derives one from the normalized
    /// skill name. Reusing an existing `skill_id` creates a new immutable
    /// version for that skill entry.
    pub fn upload_skill(
        &mut self,
        skill_id: Option<&str>,
        uploaded_by_agent_id: &str,
        uploaded_by_agent_name: Option<&str>,
        uploaded_by_agent_owner: Option<&str>,
        format: SkillFormat,
        content: &str,
    ) -> io::Result<SkillSummary> {
        let document = import_skill(content, format)?;
        document.validate()?;
        let normalized_skill_id = skill_id
            .map(normalize_skill_id)
            .transpose()?
            .unwrap_or_else(|| derive_skill_id(&document.name));
        let now = Utc::now();
        let version = SkillVersion {
            version_id: Uuid::new_v4(),
            uploaded_at: now,
            uploaded_by_agent_id: normalize_non_empty(
                uploaded_by_agent_id,
                "uploaded_by_agent_id",
            )?,
            uploaded_by_agent_name: normalize_optional(uploaded_by_agent_name),
            uploaded_by_agent_owner: normalize_optional(uploaded_by_agent_owner),
            source_format: format,
            content_hash: String::new(),
            document,
        };
        let content_hash = compute_skill_version_hash(&normalized_skill_id, &version);
        let version = SkillVersion {
            content_hash,
            ..version
        };

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
        entry.updated_at = now;
        if entry.status != SkillStatus::Revoked {
            entry.status = SkillStatus::Active;
            entry.status_reason = None;
        }
        entry.versions.push(version);
        let summary = summarize_entry(entry);
        self.rebuild_indexes();
        self.persist()?;
        Ok(summary)
    }

    /// Return all stored skills as summaries ordered by most recent update first.
    pub fn list_skills(&self) -> Vec<SkillSummary> {
        let mut summaries: Vec<_> = self.skills.values().map(summarize_entry).collect();
        summaries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        summaries
    }

    /// Search the skill registry using indexed filters plus optional text and time bounds.
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
                let summary = summarize_entry(entry);
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
    pub fn skill_versions(&self, skill_id: &str) -> io::Result<Vec<SkillVersionSummary>> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill '{skill_id}' found"),
            )
        })?;
        Ok(entry
            .versions
            .iter()
            .map(|version| SkillVersionSummary {
                skill_id: entry.skill_id.clone(),
                version_id: version.version_id,
                uploaded_at: version.uploaded_at,
                uploaded_by_agent_id: version.uploaded_by_agent_id.clone(),
                uploaded_by_agent_name: version.uploaded_by_agent_name.clone(),
                uploaded_by_agent_owner: version.uploaded_by_agent_owner.clone(),
                source_format: version.source_format,
                schema_version: version.document.schema_version,
                content_hash: version.content_hash.clone(),
            })
            .collect())
    }

    /// Return the current summary for one stored skill.
    pub fn skill_summary(&self, skill_id: &str) -> io::Result<SkillSummary> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill '{skill_id}' found"),
            )
        })?;
        Ok(summarize_entry(entry))
    }

    /// Return one stored skill version, or the latest version when omitted.
    pub fn skill_version(
        &self,
        skill_id: &str,
        version_id: Option<Uuid>,
    ) -> io::Result<SkillVersion> {
        let entry = self.skills.get(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill '{skill_id}' found"),
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
                        format!("No version '{version_id}' found for skill '{skill_id}'"),
                    )
                })?,
            None => entry.latest_version(),
        };
        Ok(version.clone())
    }

    /// Read one stored skill through the requested export adapter.
    pub fn read_skill(
        &self,
        skill_id: &str,
        version_id: Option<Uuid>,
        format: SkillFormat,
    ) -> io::Result<String> {
        let version = self.skill_version(skill_id, version_id)?;
        export_skill(&version.document, format)
    }

    /// Mark one skill as deprecated while preserving all prior versions.
    pub fn deprecate_skill(
        &mut self,
        skill_id: &str,
        reason: Option<&str>,
    ) -> io::Result<SkillSummary> {
        let entry = self.skills.get_mut(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill '{skill_id}' found"),
            )
        })?;
        entry.status = SkillStatus::Deprecated;
        entry.status_reason = normalize_optional(reason);
        entry.updated_at = Utc::now();
        let summary = summarize_entry(entry);
        self.rebuild_indexes();
        self.persist()?;
        Ok(summary)
    }

    /// Mark one skill as revoked while preserving all prior versions for auditability.
    pub fn revoke_skill(
        &mut self,
        skill_id: &str,
        reason: Option<&str>,
    ) -> io::Result<SkillSummary> {
        let entry = self.skills.get_mut(skill_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No skill '{skill_id}' found"),
            )
        })?;
        entry.status = SkillStatus::Revoked;
        entry.status_reason = normalize_optional(reason);
        entry.updated_at = Utc::now();
        let summary = summarize_entry(entry);
        self.rebuild_indexes();
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

    fn persist(&self) -> io::Result<()> {
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
        Ok(())
    }

    fn rebuild_indexes(&mut self) {
        self.indexes = SkillIndexes::from_entries(&self.skills);
    }
}

/// Import a skill file through the requested adapter into the structured object model.
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

/// Export a structured skill document through the requested adapter.
pub fn export_skill(skill: &SkillDocument, format: SkillFormat) -> io::Result<String> {
    skill.validate()?;
    match format {
        SkillFormat::Markdown => Ok(render_markdown_skill(skill)),
        SkillFormat::Json => serde_json::to_string_pretty(skill)
            .map_err(|error| io::Error::other(format!("Failed to serialize skill JSON: {error}"))),
    }
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
                                format!("Invalid skill schema_version '{value}': {error}"),
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

fn summarize_entry(entry: &SkillEntry) -> SkillSummary {
    let latest = entry.latest_version();
    SkillSummary {
        skill_id: entry.skill_id.clone(),
        name: latest.document.name.clone(),
        description: latest.document.description.clone(),
        status: entry.status,
        status_reason: entry.status_reason.clone(),
        schema_version: latest.document.schema_version,
        tags: latest.document.tags.clone(),
        triggers: latest.document.triggers.clone(),
        warnings: latest.document.warnings.clone(),
        latest_version_id: latest.version_id,
        version_count: entry.versions.len(),
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        latest_uploaded_at: latest.uploaded_at,
        latest_uploaded_by_agent_id: latest.uploaded_by_agent_id.clone(),
        latest_uploaded_by_agent_name: latest.uploaded_by_agent_name.clone(),
        latest_uploaded_by_agent_owner: latest.uploaded_by_agent_owner.clone(),
        latest_source_format: latest.source_format,
    }
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
        let latest = entry.latest_version();
        haystacks.extend(
            latest
                .document
                .sections
                .iter()
                .map(|section| section.heading.to_lowercase()),
        );
        haystacks.extend(
            latest
                .document
                .sections
                .iter()
                .map(|section| section.body.to_lowercase()),
        );
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

fn compute_skill_version_hash(skill_id: &str, version: &SkillVersion) -> String {
    #[derive(Serialize)]
    struct CanonicalSkillVersion<'a> {
        skill_id: &'a str,
        version_id: Uuid,
        uploaded_at: &'a DateTime<Utc>,
        uploaded_by_agent_id: &'a str,
        uploaded_by_agent_name: Option<&'a str>,
        uploaded_by_agent_owner: Option<&'a str>,
        source_format: SkillFormat,
        document: &'a SkillDocument,
    }

    let canonical = CanonicalSkillVersion {
        skill_id,
        version_id: version.version_id,
        uploaded_at: &version.uploaded_at,
        uploaded_by_agent_id: &version.uploaded_by_agent_id,
        uploaded_by_agent_name: version.uploaded_by_agent_name.as_deref(),
        uploaded_by_agent_owner: version.uploaded_by_agent_owner.as_deref(),
        source_format: version.source_format,
        document: &version.document,
    };
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn verify_skill_registry_integrity(skills: &BTreeMap<String, SkillEntry>) -> io::Result<()> {
    for (skill_id, entry) in skills {
        if entry.versions.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Skill '{skill_id}' has no versions"),
            ));
        }
        for version in &entry.versions {
            version.document.validate()?;
            let expected_hash = compute_skill_version_hash(skill_id, version);
            if version.content_hash != expected_hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Skill version '{}' for skill '{}' failed integrity verification",
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
