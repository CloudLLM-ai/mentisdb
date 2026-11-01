//! Writers for every historical chain format, for tests, benchmarks and fuzz
//! seeds.
//!
//! The migration engine is only as trustworthy as its coverage of the formats
//! real users still have. This module produces a deterministic chain in each of
//! them from one set of semantic thoughts, together with the thoughts a correct
//! migration must produce, so a test can assert the round trip field by field:
//!
//! ```
//! use mentisdb::migration::fixtures::{expected_after_migration, semantic_chain, write_fixture, FixtureFormat};
//! use mentisdb::migration::{run_chain_migration, MigrationOptions, MigrationOutcome};
//! use mentisdb::{chain_storage_filename, StorageAdapterKind};
//!
//! let dir = tempfile::tempdir().unwrap();
//! let thoughts = semantic_chain(5);
//! let path = dir.path().join(chain_storage_filename("demo", StorageAdapterKind::Binary));
//! write_fixture(&path, FixtureFormat::Schema1, &thoughts).unwrap();
//!
//! let outcome = run_chain_migration(dir.path(), "demo", &MigrationOptions::default()).unwrap();
//! assert!(matches!(outcome, MigrationOutcome::Migrated(_)));
//!
//! let migrated = mentisdb::MentisDb::open_with_key(dir.path(), "demo").unwrap();
//! let expected = expected_after_migration(FixtureFormat::Schema1, &thoughts);
//! assert_eq!(migrated.thoughts().len(), expected.len());
//! assert_eq!(migrated.head_hash(), expected.last().map(|t| t.hash.as_str()));
//! ```
//!
//! The legacy layouts are written with the same mirror structs the decoder
//! uses, so for formats that matter most the repository also commits files
//! produced by the real releases (see `tests/fixtures/`); this module is the
//! generator for everything else and for large synthetic chains.

use crate::{
    compute_thought_hash, compute_thought_hash_legacy, rehash_chain_to_bincode,
    LegacyThoughtRelation, LegacyThoughtRelationV2, LegacyThoughtV0, LegacyThoughtV1,
    LegacyThoughtV2, Thought, ThoughtRelation, ThoughtRelationKind, ThoughtRole, ThoughtType,
    MENTISDB_CURRENT_VERSION,
};
use chrono::{TimeZone, Utc};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use uuid::Uuid;

/// A historical on-disk chain format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixtureFormat {
    /// Binary, `schema_version` 0.
    Schema0,
    /// Binary, `schema_version` 1 (the 0.5.1 daemon).
    Schema1,
    /// Binary, `schema_version` 2 (three-field relations).
    Schema2,
    /// Binary, schema 3 before `entity_type`/`source_episode`, valid hashes.
    Schema3Base,
    /// Binary, the current layout, current hashes.
    Current,
    /// Binary, the current layout but hashed with the legacy JSON algorithm.
    CurrentLegacyHash,
    /// JSONL lines in the schema-0 layout.
    JsonlSchema0,
    /// JSONL lines in the schema-2 layout.
    JsonlSchema2,
    /// JSONL lines in the current layout with current hashes.
    JsonlCurrent,
}

impl FixtureFormat {
    /// Every format.
    pub const ALL: [FixtureFormat; 9] = [
        Self::Schema0,
        Self::Schema1,
        Self::Schema2,
        Self::Schema3Base,
        Self::Current,
        Self::CurrentLegacyHash,
        Self::JsonlSchema0,
        Self::JsonlSchema2,
        Self::JsonlCurrent,
    ];

    /// True for the JSONL formats.
    pub fn is_jsonl(self) -> bool {
        matches!(
            self,
            Self::JsonlSchema0 | Self::JsonlSchema2 | Self::JsonlCurrent
        )
    }

    /// True when a migration has nothing to do for this format.
    pub fn is_current(self) -> bool {
        self == Self::Current
    }

    fn supports_chain_key(self) -> bool {
        !matches!(self, Self::Schema0 | Self::Schema1 | Self::JsonlSchema0)
    }

    fn supports_temporal_relations(self) -> bool {
        matches!(
            self,
            Self::Schema3Base | Self::Current | Self::CurrentLegacyHash | Self::JsonlCurrent
        )
    }

    fn supports_entity_fields(self) -> bool {
        matches!(
            self,
            Self::Current | Self::CurrentLegacyHash | Self::JsonlCurrent
        )
    }
}

/// A deterministic chain of `count` thoughts exercising every field.
///
/// Hash fields are left empty; use [`expected_after_migration`] for the hashed
/// form. Relations carry a cross-chain `chain_key` on every fourth thought and
/// temporal validity on every fifth; every seventh names an entity type.
///
/// # Examples
///
/// ```
/// let chain = mentisdb::migration::fixtures::semantic_chain(10);
/// assert_eq!(chain.len(), 10);
/// assert_eq!(chain[3].index, 3);
/// ```
pub fn semantic_chain(count: usize) -> Vec<Thought> {
    let base = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    (0..count)
        .map(|i| {
            let relations = if i > 0 {
                vec![ThoughtRelation {
                    kind: if i % 3 == 0 {
                        ThoughtRelationKind::Supports
                    } else {
                        ThoughtRelationKind::References
                    },
                    target_id: Uuid::from_u128(i as u128),
                    chain_key: (i % 4 == 0).then(|| "other-chain".to_owned()),
                    valid_at: (i % 5 == 0).then_some(base),
                    invalid_at: (i % 10 == 0).then_some(base + chrono::Duration::days(1)),
                }]
            } else {
                Vec::new()
            };
            Thought {
                schema_version: MENTISDB_CURRENT_VERSION,
                id: Uuid::from_u128(i as u128 + 1),
                index: i as u64,
                timestamp: base + chrono::Duration::seconds(i as i64),
                session_id: (i % 2 == 0).then(|| Uuid::from_u128(0xfeed)),
                agent_id: format!("agent-{}", i % 3),
                signing_key_id: None,
                thought_signature: None,
                thought_type: if i % 2 == 0 {
                    ThoughtType::FactLearned
                } else {
                    ThoughtType::Decision
                },
                role: ThoughtRole::Memory,
                content: format!("fixture thought number {i} about topic {}", i % 11),
                confidence: Some(0.5 + (i % 5) as f32 / 10.0),
                importance: 0.25 + (i % 7) as f32 / 10.0,
                tags: vec![format!("tag-{}", i % 4), "fixture".to_owned()],
                concepts: vec![format!("concept-{}", i % 6)],
                refs: if i > 0 {
                    vec![i as u64 - 1]
                } else {
                    Vec::new()
                },
                relations,
                entity_type: (i % 7 == 0).then(|| "person".to_owned()),
                source_episode: (i % 9 == 0).then(|| format!("episode-{i}")),
                prev_hash: String::new(),
                hash: String::new(),
            }
        })
        .collect()
}

/// Clear the fields `format` cannot represent, as the format itself would.
pub fn project(format: FixtureFormat, thoughts: &[Thought]) -> Vec<Thought> {
    thoughts
        .iter()
        .map(|thought| {
            let mut thought = thought.clone();
            for relation in &mut thought.relations {
                if !format.supports_chain_key() {
                    relation.chain_key = None;
                }
                if !format.supports_temporal_relations() {
                    relation.valid_at = None;
                    relation.invalid_at = None;
                }
            }
            if !format.supports_entity_fields() {
                thought.entity_type = None;
                thought.source_episode = None;
            }
            thought
        })
        .collect()
}

/// The thoughts a correct migration of `format` must yield: the projection with
/// the current hash chain rebuilt and the current `schema_version`.
pub fn expected_after_migration(format: FixtureFormat, thoughts: &[Thought]) -> Vec<Thought> {
    let mut expected = project(format, thoughts);
    rehash_chain_to_bincode(&mut expected);
    expected
}

fn legacy_v0(thought: &Thought, schema_version: u32) -> LegacyThoughtV0 {
    LegacyThoughtV0 {
        schema_version,
        id: thought.id,
        index: thought.index,
        timestamp: thought.timestamp,
        session_id: thought.session_id,
        agent_id: thought.agent_id.clone(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: thought.thought_type,
        role: thought.role,
        content: thought.content.clone(),
        confidence: thought.confidence,
        importance: thought.importance,
        tags: thought.tags.clone(),
        concepts: thought.concepts.clone(),
        refs: thought.refs.clone(),
        relations: thought
            .relations
            .iter()
            .map(|r| LegacyThoughtRelation {
                kind: r.kind,
                target_id: r.target_id,
            })
            .collect(),
        prev_hash: format!("legacy-prev-{}", thought.index),
        hash: format!("legacy-hash-{}", thought.index),
    }
}

fn legacy_v1(thought: &Thought) -> LegacyThoughtV1 {
    LegacyThoughtV1 {
        schema_version: 2,
        id: thought.id,
        index: thought.index,
        timestamp: thought.timestamp,
        session_id: thought.session_id,
        agent_id: thought.agent_id.clone(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: thought.thought_type,
        role: thought.role,
        content: thought.content.clone(),
        confidence: thought.confidence,
        importance: thought.importance,
        tags: thought.tags.clone(),
        concepts: thought.concepts.clone(),
        refs: thought.refs.clone(),
        relations: thought
            .relations
            .iter()
            .map(|r| LegacyThoughtRelationV2 {
                kind: r.kind,
                target_id: r.target_id,
                chain_key: r.chain_key.clone(),
            })
            .collect(),
        prev_hash: format!("legacy-prev-{}", thought.index),
        hash: format!("legacy-hash-{}", thought.index),
    }
}

fn legacy_v2_base(thought: &Thought) -> LegacyThoughtV2 {
    LegacyThoughtV2 {
        schema_version: 3,
        id: thought.id,
        index: thought.index,
        timestamp: thought.timestamp,
        session_id: thought.session_id,
        agent_id: thought.agent_id.clone(),
        signing_key_id: thought.signing_key_id.clone(),
        thought_signature: thought.thought_signature.clone(),
        thought_type: thought.thought_type,
        role: thought.role,
        content: thought.content.clone(),
        confidence: thought.confidence,
        importance: thought.importance,
        tags: thought.tags.clone(),
        concepts: thought.concepts.clone(),
        refs: thought.refs.clone(),
        relations: thought.relations.clone(),
        prev_hash: thought.prev_hash.clone(),
        hash: thought.hash.clone(),
    }
}

/// Re-link `thoughts` with the legacy JSON hash algorithm (stored hashes chain
/// through the legacy hash of the previous record).
fn legacy_hash_chain(thoughts: &mut [Thought]) {
    let mut previous = String::new();
    for thought in thoughts {
        thought.prev_hash = previous.clone();
        thought.hash = compute_thought_hash_legacy(thought);
        previous = thought.hash.clone();
    }
}

fn write_record(writer: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    writer.write_all(&(payload.len() as u64).to_le_bytes())?;
    writer.write_all(payload)
}

fn encode<T: serde::Serialize>(value: &T) -> io::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(value, bincode::config::standard())
        .map_err(|error| io::Error::other(error.to_string()))
}

/// Write `thoughts` to `path` exactly as a release that used `format` would have.
///
/// `thoughts` is a [`semantic_chain`] (or any chain in the current layout);
/// fields the format cannot represent are dropped, and hashes are generated the
/// way that release generated them.
///
/// # Errors
///
/// Filesystem errors.
pub fn write_fixture(path: &Path, format: FixtureFormat, thoughts: &[Thought]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut writer = BufWriter::new(File::create(path)?);
    let projected = project(format, thoughts);
    match format {
        FixtureFormat::Schema0 | FixtureFormat::Schema1 => {
            let version = u32::from(format == FixtureFormat::Schema1);
            for thought in &projected {
                write_record(&mut writer, &encode(&legacy_v0(thought, version))?)?;
            }
        }
        FixtureFormat::Schema2 => {
            for thought in &projected {
                write_record(&mut writer, &encode(&legacy_v1(thought))?)?;
            }
        }
        FixtureFormat::Schema3Base => {
            let hashed = expected_after_migration(format, thoughts);
            for thought in &hashed {
                write_record(&mut writer, &encode(&legacy_v2_base(thought))?)?;
            }
        }
        FixtureFormat::Current => {
            for thought in &expected_after_migration(format, thoughts) {
                write_record(&mut writer, &encode(thought)?)?;
            }
        }
        FixtureFormat::CurrentLegacyHash => {
            let mut hashed = projected;
            legacy_hash_chain(&mut hashed);
            for thought in &hashed {
                write_record(&mut writer, &encode(thought)?)?;
            }
        }
        FixtureFormat::JsonlSchema0 => {
            for thought in &projected {
                let line =
                    serde_json::to_string(&legacy_v0(thought, 0)).map_err(io::Error::other)?;
                writeln!(writer, "{line}")?;
            }
        }
        FixtureFormat::JsonlSchema2 => {
            for thought in &projected {
                let line = serde_json::to_string(&legacy_v1(thought)).map_err(io::Error::other)?;
                writeln!(writer, "{line}")?;
            }
        }
        FixtureFormat::JsonlCurrent => {
            for thought in &expected_after_migration(format, thoughts) {
                let line = serde_json::to_string(thought).map_err(io::Error::other)?;
                writeln!(writer, "{line}")?;
            }
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()
}

/// Write a large synthetic chain in `format` without holding it in memory.
///
/// Records are generated one at a time with short, varied content, so a million
/// thoughts cost a few hundred megabytes on disk and constant RAM. Used by the
/// large-chain timing test and bench.
///
/// # Errors
///
/// Filesystem errors.
pub fn write_large_fixture(path: &Path, format: FixtureFormat, count: usize) -> io::Result<()> {
    assert!(
        matches!(
            format,
            FixtureFormat::Schema1 | FixtureFormat::Schema3Base | FixtureFormat::Current
        ),
        "large fixtures are supported for Schema1, Schema3Base and Current"
    );
    let mut writer = BufWriter::with_capacity(1 << 20, File::create(path)?);
    let mut previous = String::new();
    let base = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
    for i in 0..count {
        let mut thought = Thought {
            schema_version: MENTISDB_CURRENT_VERSION,
            id: Uuid::from_u128(i as u128 + 1),
            index: i as u64,
            timestamp: base + chrono::Duration::seconds(i as i64),
            session_id: None,
            agent_id: format!("agent-{}", i % 8),
            signing_key_id: None,
            thought_signature: None,
            thought_type: ThoughtType::FactLearned,
            role: ThoughtRole::Memory,
            content: format!(
                "synthetic memory {i}: the deployment of service {} finished with status {}",
                i % 97,
                i % 5
            ),
            confidence: Some(0.9),
            importance: 0.5,
            tags: vec![format!("svc-{}", i % 97)],
            concepts: vec!["deployment".to_owned()],
            refs: if i > 0 {
                vec![i as u64 - 1]
            } else {
                Vec::new()
            },
            relations: Vec::new(),
            entity_type: None,
            source_episode: None,
            prev_hash: String::new(),
            hash: String::new(),
        };
        match format {
            FixtureFormat::Schema1 => {
                write_record(&mut writer, &encode(&legacy_v0(&thought, 1))?)?;
            }
            _ => {
                thought.prev_hash = previous.clone();
                thought.hash = compute_thought_hash(&thought);
                previous = thought.hash.clone();
                if format == FixtureFormat::Schema3Base {
                    write_record(&mut writer, &encode(&legacy_v2_base(&thought))?)?;
                } else {
                    write_record(&mut writer, &encode(&thought)?)?;
                }
            }
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()
}

/// Re-encode an existing chain in the schema-3 base layout (no `entity_type` or
/// `source_episode`), keeping hashes and signatures exactly as they are.
///
/// Lets a test take a chain produced by the real library (with real signatures)
/// and present it to the engine as a chain written before the entity fields
/// existed. The entity fields of `thoughts` must be `None`: the hash does not
/// cover them, but the layout cannot store them.
///
/// # Errors
///
/// Filesystem errors; `InvalidInput` when a thought has entity fields set.
pub fn write_schema3_base_as_is(path: &Path, thoughts: &[Thought]) -> io::Result<()> {
    if thoughts
        .iter()
        .any(|t| t.entity_type.is_some() || t.source_episode.is_some())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the schema-3 base layout cannot store entity fields",
        ));
    }
    let mut writer = BufWriter::new(File::create(path)?);
    for thought in thoughts {
        write_record(&mut writer, &encode(&legacy_v2_base(thought))?)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()
}
