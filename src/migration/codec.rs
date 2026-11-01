//! Record codec for the migration engine: decode any historical thought layout,
//! upgrade it one version at a time, and rebuild the hash chain while proving
//! that nothing was tampered with on the way.
//!
//! The decoders here mirror the layouts of every release that ever wrote a
//! chain file, including the three layouts that all carry `schema_version` 3
//! (base, with `entity_type`, current) and the alias `schema_version` 4 that
//! March and April 2026 builds stamped on schema-3 records
//! ([`crate::MENTISDB_LEGACY_SCHEMA_V4_ALIAS`]). A record is dispatched on its
//! leading `schema_version` varint, each layout must consume the whole payload
//! and carry plausible hashes (a torn record can otherwise decode as another
//! layout with shifted fields), and the record is then walked through
//! [`UPGRADE_STEPS`] until it is the current [`Thought`] layout. The next thought
//! schema is 5: add a decoder arm and a step, leaving the existing ones alone.
//!
//! Hash handling is deliberately strict. A record from schema 3 (the first
//! layout that stored trustworthy hashes) must verify under the rule that
//! produced it: the current bincode algorithm over the rebuilt chain, the
//! legacy JSON algorithm over the stored links, or, for records stamped with the
//! alias, the current algorithm with the *stored* `schema_version` over the
//! stored links. Anything else is [`MigrationError::Integrity`] and the migration
//! is refused. Records from schemas 0 to 2 stored hashes under older canonical
//! forms that cannot be recomputed, so their stored links (each `prev_hash`
//! equals the previous record's stored hash) are verified before the hashes are
//! rebuilt; an edit that keeps a record's stored hash is not detectable there. A
//! migration therefore never turns a removed, inserted or reordered record, or
//! a corrupted trusted record, into a freshly valid chain.

use super::MigrationError;
use crate::signing::{SignatureVersion, StoredSignature};
use crate::{
    compute_thought_hash, compute_thought_hash_legacy, LegacyThoughtV0, LegacyThoughtV1,
    LegacyThoughtV2, LegacyThoughtV3b, Thought, ThoughtRelation, MENTISDB_CURRENT_VERSION,
};

/// One `vN -> vN+1` step of the upgrade pipeline.
///
/// The table [`UPGRADE_STEPS`] is the authoritative, ordered list of what the
/// engine knows how to do; reports and the journal name steps by [`Self::name`].
///
/// # Examples
///
/// ```
/// use mentisdb::migration::UPGRADE_STEPS;
///
/// assert_eq!(UPGRADE_STEPS.first().unwrap().from_layout, "schema0-1");
/// assert!(UPGRADE_STEPS.iter().all(|step| !step.description.is_empty()));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpgradeStep {
    /// Stable machine-readable name, recorded in journals and reports.
    pub name: &'static str,
    /// Layout the step reads.
    pub from_layout: &'static str,
    /// Layout the step produces.
    pub to_layout: &'static str,
    /// What changes, for humans.
    pub description: &'static str,
}

/// Every upgrade step, oldest first.
pub const UPGRADE_STEPS: &[UpgradeStep] = &[
    UpgradeStep {
        name: "relations-v0-to-v2",
        from_layout: "schema0-1",
        to_layout: "schema2",
        description:
            "relations gain an optional cross-chain chain_key; legacy hashes are discarded",
    },
    UpgradeStep {
        name: "relations-v2-to-v3",
        from_layout: "schema2",
        to_layout: "schema3-base",
        description: "relations gain valid_at and invalid_at temporal validity",
    },
    UpgradeStep {
        name: "thought-add-entity-type",
        from_layout: "schema3-base",
        to_layout: "schema3-entity",
        description: "thoughts gain entity_type",
    },
    UpgradeStep {
        name: "thought-add-source-episode",
        from_layout: "schema3-entity",
        to_layout: "schema3",
        description: "thoughts gain source_episode",
    },
];

/// The historical on-disk layout a record was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordLayout {
    /// Schema 0 and 1: two-field relations, hashes not trustworthy.
    Schema0To1,
    /// Schema 2: three-field relations, hashes not trustworthy.
    Schema2,
    /// Schema 3 before `entity_type`/`source_episode` existed.
    Schema3Base,
    /// Schema 3 with `entity_type` but before `source_episode` existed.
    Schema3Entity,
    /// The current layout.
    Current,
}

impl RecordLayout {
    /// True when the layout stored hashes that cannot be verified and must be rebuilt.
    pub fn hashes_untrusted(self) -> bool {
        matches!(self, Self::Schema0To1 | Self::Schema2)
    }

    /// The `schema_version` the layout is stored under (schema 0 and 1 share one layout).
    pub fn schema_version(self) -> u32 {
        match self {
            Self::Schema0To1 => 0,
            Self::Schema2 => 2,
            Self::Schema3Base | Self::Schema3Entity | Self::Current => 3,
        }
    }
}

/// A decoded record plus where it came from.
#[derive(Debug)]
pub struct DecodedRecord {
    /// The record upgraded to the current layout (hash fields not yet rebuilt).
    pub thought: Thought,
    /// The layout it was written in.
    pub layout: RecordLayout,
    /// Names of the [`UPGRADE_STEPS`] applied, oldest first.
    pub steps: Vec<&'static str>,
    /// The `schema_version` stored in the record (before normalization). It is
    /// part of what the stored hash covers, so the hash rebuilder needs it.
    pub stored_schema: u32,
}

fn invalid(message: impl Into<String>) -> MigrationError {
    MigrationError::Corrupt {
        index: None,
        message: message.into(),
    }
}

/// Read the leading `schema_version` varint of a binary record payload.
///
/// # Errors
///
/// [`MigrationError::Corrupt`] when the payload does not start with a varint.
pub fn peek_schema_version(payload: &[u8]) -> Result<u32, MigrationError> {
    let (version, _): (u32, usize) =
        bincode::decode_from_slice(payload, crate::decode_limits::thought_record_config())
            .map_err(|error| invalid(format!("cannot read schema version: {error}")))?;
    Ok(version)
}

/// Decode one binary record payload written by any release and upgrade it to
/// the current layout, one step at a time.
///
/// # Errors
///
/// [`MigrationError::FutureSchema`] for a `schema_version` newer than this
/// build understands (the caller must upgrade mentisdb, never guess), and
/// [`MigrationError::Corrupt`] when the bytes do not decode in the layout
/// their version announces.
pub fn decode_and_upgrade(payload: &[u8]) -> Result<DecodedRecord, MigrationError> {
    let version = peek_schema_version(payload)?;
    let config = crate::decode_limits::thought_record_config();
    let mut steps = Vec::new();
    match version {
        0 | 1 => {
            let (legacy, _): (LegacyThoughtV0, usize) =
                bincode::serde::decode_from_slice(payload, config)
                    .map_err(|e| invalid(format!("schema {version} record: {e}")))?;
            let v2 = step_v0_to_v2(legacy, &mut steps);
            let base = step_v2_to_v3(v2, &mut steps);
            Ok(DecodedRecord {
                thought: {
                    let with_entity = step_add_entity_type(base, &mut steps);
                    step_add_source_episode(with_entity, &mut steps)
                },
                layout: RecordLayout::Schema0To1,
                steps,
                stored_schema: version,
            })
        }
        2 => {
            let (legacy, _): (LegacyThoughtV1, usize) =
                bincode::serde::decode_from_slice(payload, config)
                    .map_err(|e| invalid(format!("schema 2 record: {e}")))?;
            let base = step_v2_to_v3(legacy, &mut steps);
            Ok(DecodedRecord {
                thought: {
                    let with_entity = step_add_entity_type(base, &mut steps);
                    step_add_source_episode(with_entity, &mut steps)
                },
                layout: RecordLayout::Schema2,
                steps,
                stored_schema: version,
            })
        }
        3 | crate::MENTISDB_LEGACY_SCHEMA_V4_ALIAS => {
            // Schema 3 (and its alias 4, see
            // `crate::MENTISDB_LEGACY_SCHEMA_V4_ALIAS`) exists in three
            // layouts. Each must consume the whole payload: a layout that
            // merely decodes without using every byte read the fields of
            // another one, shifted.
            if let Ok((thought, used)) =
                bincode::serde::decode_from_slice::<Thought, _>(payload, config)
            {
                if used == payload.len() {
                    return Ok(DecodedRecord {
                        thought,
                        layout: RecordLayout::Current,
                        steps,
                        stored_schema: version,
                    });
                }
            }
            if let Ok((with_entity, used)) =
                bincode::serde::decode_from_slice::<LegacyThoughtV3b, _>(payload, config)
            {
                if used == payload.len()
                    && crate::plausible_schema3_hashes(
                        with_entity.index,
                        &with_entity.prev_hash,
                        &with_entity.hash,
                    )
                {
                    return Ok(DecodedRecord {
                        thought: step_add_source_episode(with_entity, &mut steps),
                        layout: RecordLayout::Schema3Entity,
                        steps,
                        stored_schema: version,
                    });
                }
            }
            let (base, used): (LegacyThoughtV2, usize) =
                bincode::serde::decode_from_slice(payload, config)
                    .map_err(|e| invalid(format!("schema 3 record: {e}")))?;
            if used != payload.len()
                || !crate::plausible_schema3_hashes(base.index, &base.prev_hash, &base.hash)
            {
                return Err(invalid(format!(
                    "schema 3 record matches no known layout ({} of {} bytes used)",
                    used,
                    payload.len()
                )));
            }
            let with_entity = step_add_entity_type(base, &mut steps);
            Ok(DecodedRecord {
                thought: step_add_source_episode(with_entity, &mut steps),
                layout: RecordLayout::Schema3Base,
                steps,
                stored_schema: version,
            })
        }
        newer => Err(MigrationError::FutureSchema { version: newer }),
    }
}

/// Schema 0/1 to schema 2: relations become three-field. The stored hashes are
/// carried along so the rebuilder can check the old chain's linkage before
/// they are replaced.
fn step_v0_to_v2(legacy: LegacyThoughtV0, steps: &mut Vec<&'static str>) -> LegacyThoughtV1 {
    steps.push("relations-v0-to-v2");
    LegacyThoughtV1 {
        schema_version: legacy.schema_version,
        id: legacy.id,
        index: legacy.index,
        timestamp: legacy.timestamp,
        session_id: legacy.session_id,
        agent_id: legacy.agent_id,
        signing_key_id: legacy.signing_key_id,
        thought_signature: legacy.thought_signature,
        thought_type: legacy.thought_type,
        role: legacy.role,
        content: legacy.content,
        confidence: legacy.confidence,
        importance: legacy.importance,
        tags: legacy.tags,
        concepts: legacy.concepts,
        refs: legacy.refs,
        relations: legacy
            .relations
            .into_iter()
            .map(|relation| crate::LegacyThoughtRelationV2 {
                kind: relation.kind,
                target_id: relation.target_id,
                chain_key: None,
            })
            .collect(),
        prev_hash: legacy.prev_hash,
        hash: legacy.hash,
    }
}

/// Schema 2 to the schema 3 base layout: relations become five-field.
fn step_v2_to_v3(legacy: LegacyThoughtV1, steps: &mut Vec<&'static str>) -> LegacyThoughtV2 {
    steps.push("relations-v2-to-v3");
    LegacyThoughtV2 {
        schema_version: legacy.schema_version,
        id: legacy.id,
        index: legacy.index,
        timestamp: legacy.timestamp,
        session_id: legacy.session_id,
        agent_id: legacy.agent_id,
        signing_key_id: legacy.signing_key_id,
        thought_signature: legacy.thought_signature,
        thought_type: legacy.thought_type,
        role: legacy.role,
        content: legacy.content,
        confidence: legacy.confidence,
        importance: legacy.importance,
        tags: legacy.tags,
        concepts: legacy.concepts,
        refs: legacy.refs,
        relations: legacy
            .relations
            .into_iter()
            .map(ThoughtRelation::from)
            .collect(),
        prev_hash: legacy.prev_hash,
        hash: legacy.hash,
    }
}

/// Schema 3 base layout to the layout with `entity_type`.
fn step_add_entity_type(base: LegacyThoughtV2, steps: &mut Vec<&'static str>) -> LegacyThoughtV3b {
    steps.push("thought-add-entity-type");
    LegacyThoughtV3b {
        schema_version: base.schema_version,
        id: base.id,
        index: base.index,
        timestamp: base.timestamp,
        session_id: base.session_id,
        agent_id: base.agent_id,
        signing_key_id: base.signing_key_id,
        thought_signature: base.thought_signature,
        thought_type: base.thought_type,
        role: base.role,
        content: base.content,
        confidence: base.confidence,
        importance: base.importance,
        tags: base.tags,
        concepts: base.concepts,
        refs: base.refs,
        relations: base.relations,
        entity_type: None,
        prev_hash: base.prev_hash,
        hash: base.hash,
    }
}

/// Schema 3 with `entity_type` to the current layout.
fn step_add_source_episode(
    with_entity: LegacyThoughtV3b,
    steps: &mut Vec<&'static str>,
) -> Thought {
    steps.push("thought-add-source-episode");
    Thought {
        schema_version: MENTISDB_CURRENT_VERSION,
        id: with_entity.id,
        index: with_entity.index,
        timestamp: with_entity.timestamp,
        session_id: with_entity.session_id,
        agent_id: with_entity.agent_id,
        signing_key_id: with_entity.signing_key_id,
        thought_signature: with_entity.thought_signature,
        thought_type: with_entity.thought_type,
        role: with_entity.role,
        content: with_entity.content,
        confidence: with_entity.confidence,
        importance: with_entity.importance,
        tags: with_entity.tags,
        concepts: with_entity.concepts,
        refs: with_entity.refs,
        relations: with_entity.relations,
        entity_type: with_entity.entity_type,
        source_episode: None,
        prev_hash: with_entity.prev_hash,
        hash: with_entity.hash,
    }
}

/// Rebuilds the hash chain record by record while verifying what was stored.
///
/// Feed upgraded thoughts in chain order to [`Self::apply`]; the tracker
/// carries the previous hash, checks every trustworthy stored hash, and records
/// whether any hash changed and how many signed thoughts were seen.
#[derive(Debug, Default)]
pub struct HashRebuilder {
    next_index: u64,
    previous_new_hash: String,
    previous_stored_hash: String,
    /// True when at least one record's hash differs from what was stored.
    pub hash_changed: bool,
    /// Number of thoughts that carry a signature.
    pub signed: u64,
    /// Records from schemas 0 to 2 whose stored hash links were checked
    /// (`prev_hash` equals the previous record's stored hash) before the
    /// hashes were rebuilt. The old canonical form cannot be recomputed, but a
    /// deleted, inserted, reordered or edited-link record is caught here.
    pub legacy_links_verified: u64,
}

impl HashRebuilder {
    /// Hash of the last record applied (empty before the first), i.e. the new chain head.
    pub fn head_hash(&self) -> &str {
        &self.previous_new_hash
    }

    /// Verify one upgraded record and return it with `prev_hash`/`hash` rebuilt.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Integrity`] when the record's index is out of
    ///   sequence, or when a schema-3 record's stored hash matches neither the
    ///   current nor the legacy algorithm (corruption or tampering).
    /// - [`MigrationError::SignatureWouldBreak`] when rebuilding would change
    ///   the `prev_hash` a v2 signature is bound to; migrating would silently
    ///   invalidate a signature, so the chain is left untouched instead.
    pub fn apply(
        &mut self,
        mut thought: Thought,
        layout: RecordLayout,
        stored_schema: u32,
    ) -> Result<Thought, MigrationError> {
        if thought.index != self.next_index {
            return Err(MigrationError::Integrity {
                index: thought.index,
                message: format!(
                    "record at position {} carries index {}",
                    self.next_index, thought.index
                ),
            });
        }
        let stored_prev = std::mem::take(&mut thought.prev_hash);
        let stored_hash = std::mem::take(&mut thought.hash);
        thought.schema_version = MENTISDB_CURRENT_VERSION;
        thought.prev_hash = self.previous_new_hash.clone();
        let new_hash = compute_thought_hash(&thought);

        if layout.hashes_untrusted() && !stored_hash.is_empty() {
            if stored_prev != self.previous_stored_hash {
                return Err(MigrationError::Integrity {
                    index: thought.index,
                    message: "the old chain's prev_hash does not link to the previous record's \
                              stored hash; a record was removed, inserted or altered before \
                              this migration"
                        .to_string(),
                });
            }
            self.legacy_links_verified += 1;
        }
        if !layout.hashes_untrusted() {
            let current_ok = stored_hash == new_hash;
            let legacy_ok = stored_prev == self.previous_stored_hash && {
                let mut as_stored = thought.clone();
                as_stored.prev_hash = stored_prev.clone();
                stored_hash == compute_thought_hash_legacy(&as_stored)
            };
            // Builds that stamped schema 4 on schema-3 records hashed them with
            // the stored `schema_version`, and linked each record to the
            // previous stored hash. Verify in that domain too.
            let stored_domain_ok = stored_schema != MENTISDB_CURRENT_VERSION
                && stored_prev == self.previous_stored_hash
                && {
                    let mut as_stored = thought.clone();
                    as_stored.schema_version = stored_schema;
                    as_stored.prev_hash = stored_prev.clone();
                    stored_hash == compute_thought_hash(&as_stored)
                };
            if !current_ok && !legacy_ok && !stored_domain_ok {
                return Err(MigrationError::Integrity {
                    index: thought.index,
                    message: "stored hash matches neither the current nor the legacy hash \
                              algorithm; the chain is corrupt or was tampered with"
                        .to_string(),
                });
            }
        }

        let prev_changed = layout.hashes_untrusted() || stored_prev != thought.prev_hash;
        if let Some(raw) = thought.thought_signature.as_deref() {
            self.signed += 1;
            let bound_to_prev = StoredSignature::parse(raw)
                .map(|stored| stored.version == SignatureVersion::V2)
                .unwrap_or(false);
            if bound_to_prev && prev_changed {
                return Err(MigrationError::SignatureWouldBreak {
                    index: thought.index,
                });
            }
        }
        self.hash_changed |= layout.hashes_untrusted()
            || stored_schema != MENTISDB_CURRENT_VERSION
            || stored_hash != new_hash
            || prev_changed;

        thought.hash = new_hash.clone();
        self.previous_stored_hash = stored_hash;
        self.previous_new_hash = new_hash;
        self.next_index += 1;
        Ok(thought)
    }
}

/// Decode one JSONL line written by any release and upgrade it.
///
/// JSONL chains are read-only legacy storage. Lines are dispatched on their
/// `schema_version` (absent means schema 0) exactly like binary records, so a
/// schema-3 line keeps its temporal relation fields instead of being squeezed
/// through the oldest layout.
///
/// # Errors
///
/// [`MigrationError::FutureSchema`] for a newer schema, and
/// [`MigrationError::Corrupt`] for a line that is not a thought in the layout
/// its version announces.
pub fn decode_json_and_upgrade(line: &str) -> Result<DecodedRecord, MigrationError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| invalid(format!("unparseable JSONL record: {error}")))?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let mut steps = Vec::new();
    let corrupt =
        |error: serde_json::Error| invalid(format!("schema {version} JSONL record: {error}"));
    match version {
        0 | 1 => {
            let legacy: LegacyThoughtV0 = serde_json::from_value(value).map_err(corrupt)?;
            let v2 = step_v0_to_v2(legacy, &mut steps);
            let base = step_v2_to_v3(v2, &mut steps);
            Ok(DecodedRecord {
                thought: {
                    let with_entity = step_add_entity_type(base, &mut steps);
                    step_add_source_episode(with_entity, &mut steps)
                },
                layout: RecordLayout::Schema0To1,
                steps,
                stored_schema: version,
            })
        }
        2 => {
            let legacy: LegacyThoughtV1 = serde_json::from_value(value).map_err(corrupt)?;
            let base = step_v2_to_v3(legacy, &mut steps);
            Ok(DecodedRecord {
                thought: {
                    let with_entity = step_add_entity_type(base, &mut steps);
                    step_add_source_episode(with_entity, &mut steps)
                },
                layout: RecordLayout::Schema2,
                steps,
                stored_schema: version,
            })
        }
        3 | crate::MENTISDB_LEGACY_SCHEMA_V4_ALIAS => {
            let thought: Thought = serde_json::from_value(value).map_err(corrupt)?;
            Ok(DecodedRecord {
                thought,
                layout: RecordLayout::Current,
                steps,
                stored_schema: version,
            })
        }
        newer => Err(MigrationError::FutureSchema { version: newer }),
    }
}
