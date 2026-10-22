//! Regression tests for the on-open legacy-hash rewrite of binary chains.

use chrono::{DateTime, Utc};
use mentisdb::{MentisDb, Thought, ThoughtRelation, ThoughtRole, ThoughtType};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use tempfile::tempdir;
use uuid::Uuid;

/// Field order matters: the legacy hash is over declaration-ordered JSON.
#[derive(Serialize)]
struct Canonical<'a> {
    schema_version: u32,
    id: Uuid,
    index: u64,
    timestamp: &'a DateTime<Utc>,
    session_id: Option<Uuid>,
    agent_id: &'a str,
    signing_key_id: Option<&'a str>,
    thought_signature: Option<&'a [u8]>,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: &'a str,
    confidence: Option<f32>,
    importance: f32,
    tags: &'a [String],
    concepts: &'a [String],
    refs: &'a [u64],
    relations: &'a [ThoughtRelation],
    prev_hash: &'a str,
}

/// Legacy JSON-based canonical hash, mirroring the pre-bincode algorithm.
fn legacy_hash(thought: &Thought) -> String {
    let canonical = Canonical {
        schema_version: thought.schema_version,
        id: thought.id,
        index: thought.index,
        timestamp: &thought.timestamp,
        session_id: thought.session_id,
        agent_id: &thought.agent_id,
        signing_key_id: thought.signing_key_id.as_deref(),
        thought_signature: thought.thought_signature.as_deref(),
        thought_type: thought.thought_type,
        role: thought.role,
        content: &thought.content,
        confidence: thought.confidence,
        importance: thought.importance,
        tags: &thought.tags,
        concepts: &thought.concepts,
        refs: &thought.refs,
        relations: &thought.relations,
        prev_hash: &thought.prev_hash,
    };
    let digest = Sha256::digest(serde_json::to_vec(&canonical).unwrap());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A chain carrying legacy hashes is rewritten on open with bincode hashes,
/// loads cleanly afterwards, and leaves no temp files behind.
#[test]
fn test_legacy_hash_chain_is_rewritten_atomically_on_open() {
    let temp = tempdir().unwrap();
    let path = {
        let mut chain = MentisDb::open_with_key(temp.path(), "legacy").unwrap();
        for i in 0..3 {
            chain
                .append("agent", ThoughtType::Finding, &format!("thought {i}"))
                .unwrap();
        }
        chain.flush().unwrap();
        std::path::PathBuf::from(chain.storage_location())
    };
    let mut thoughts = MentisDb::open_with_key(temp.path(), "legacy")
        .unwrap()
        .thoughts()
        .to_vec();
    let mut prev = String::new();
    for thought in &mut thoughts {
        thought.prev_hash = prev.clone();
        thought.hash = legacy_hash(thought);
        prev = thought.hash.clone();
    }
    let mut bytes = Vec::new();
    for thought in &thoughts {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();

    let chain = MentisDb::open_with_key(temp.path(), "legacy").unwrap();
    assert_eq!(chain.thoughts().len(), 3);
    assert!(chain.verify_integrity());
    assert_ne!(chain.thoughts()[0].hash, thoughts[0].hash);
    drop(chain);

    let reopened = MentisDb::open_with_key(temp.path(), "legacy").unwrap();
    assert!(reopened.verify_integrity());
    let stray = std::fs::read_dir(temp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(stray, 0);
}
