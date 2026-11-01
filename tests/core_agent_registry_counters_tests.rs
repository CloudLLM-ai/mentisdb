//! Regression tests for the agent registry file: per-agent activity counters
//! are derived from the chain on every open, so appends that do not change an
//! agent's identity must not rewrite `*.agents.json`.

use mentisdb::{MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Locate the single `*.agents.json` file in `dir`.
fn agents_file(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .expect("chain dir should be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".agents.json"))
        .expect("an agents registry file should exist")
}

/// Read the persisted `thought_count` of `agent` straight from the file.
fn persisted_count(dir: &Path, agent: &str) -> u64 {
    let text = fs::read_to_string(agents_file(dir)).expect("registry should be readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("registry is JSON");
    value["agents"][agent]["thought_count"]
        .as_u64()
        .expect("thought_count should be present")
}

/// Appends by a known agent below the flush threshold leave the file alone,
/// while the in-memory registry and a reopened chain report exact counts.
#[test]
fn test_counter_only_appends_do_not_rewrite_agent_registry() {
    let dir = TempDir::new().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought("a", ThoughtInput::new(ThoughtType::FactLearned, "first"))
        .unwrap();
    assert_eq!(persisted_count(dir.path(), "a"), 1);
    for n in 0..5 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::FactLearned, format!("t{n}")),
            )
            .unwrap();
    }
    assert_eq!(
        persisted_count(dir.path(), "a"),
        1,
        "counter-only appends must not rewrite the registry"
    );
    drop(chain);
    assert_eq!(persisted_count(dir.path(), "a"), 6, "Drop flushes counters");
    let reopened =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    assert_eq!(reopened.agent_registry().agents["a"].thought_count, 6);
}

/// A crash (registry never flushed after counter-only appends) loses nothing:
/// the counts are recomputed from the chain at open.
#[test]
fn test_counters_recomputed_from_chain_after_stale_registry() {
    let dir = TempDir::new().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    for n in 0..4 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::FactLearned, format!("t{n}")),
            )
            .unwrap();
    }
    std::mem::forget(chain); // simulate a kill: no Drop flush
    assert_eq!(persisted_count(dir.path(), "a"), 1);
    let reopened =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    let record = &reopened.agent_registry().agents["a"];
    assert_eq!(record.thought_count, 4);
    assert_eq!(record.last_seen_index, Some(3));
}

/// A new agent is an identity change and is persisted immediately.
#[test]
fn test_new_agent_is_persisted_immediately() {
    let dir = TempDir::new().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought("a", ThoughtInput::new(ThoughtType::FactLearned, "x"))
        .unwrap();
    chain
        .append_thought("b", ThoughtInput::new(ThoughtType::FactLearned, "y"))
        .unwrap();
    assert_eq!(persisted_count(dir.path(), "b"), 1);
}

/// Crossing the flush threshold writes the counters out.
#[test]
fn test_threshold_flushes_counters() {
    let dir = TempDir::new().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "c", StorageAdapterKind::Binary)
            .unwrap();
    for n in 0..(mentisdb::FLUSH_THRESHOLD + 2) {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::FactLearned, format!("t{n}")),
            )
            .unwrap();
    }
    assert!(persisted_count(dir.path(), "a") > 1);
    std::mem::forget(chain);
}
