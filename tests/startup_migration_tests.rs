use mentisdb::{
    migrate_chain_hash_algorithm, migrate_registered_chains_with_adapter,
    refresh_registered_chain_counts, MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_startup_migration_test_{}_{}_{}",
        std::process::id(),
        n,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn refresh_registered_chain_counts_uses_fast_metadata_scan() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "alpha", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "agent-a",
            ThoughtInput::new(ThoughtType::FactLearned, "first thought"),
        )
        .unwrap();
    chain
        .append_thought(
            "agent-b",
            ThoughtInput::new(ThoughtType::FactLearned, "second thought"),
        )
        .unwrap();
    drop(chain);

    refresh_registered_chain_counts(&dir).unwrap();

    let registry = mentisdb::load_registered_chains(&dir).unwrap();
    let entry = registry
        .chains
        .get("alpha")
        .expect("alpha chain should be registered");
    assert_eq!(entry.thought_count, 2);
    assert_eq!(entry.agent_count, 2);
}

#[test]
fn startup_migration_pass_is_noop_for_current_binary_chains() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "beta", StorageAdapterKind::Binary).unwrap();
    chain
        .append_thought("agent-a", ThoughtInput::new(ThoughtType::Summary, "seed"))
        .unwrap();
    drop(chain);

    let reports =
        migrate_registered_chains_with_adapter(&dir, StorageAdapterKind::Binary, |_| {}).unwrap();
    assert!(reports.is_empty());

    let rehashed = migrate_chain_hash_algorithm(&dir, |_| {}).unwrap();
    assert_eq!(rehashed, 0);
}
