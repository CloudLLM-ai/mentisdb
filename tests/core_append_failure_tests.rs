//! Regression tests for failure handling after the canonical write and for
//! concurrent derived-state flushes.

use mentisdb::search::{sidecar_wal_path, LocalTextEmbeddingProvider};
use mentisdb::{MentisDb, ThoughtType};
use std::sync::{Arc, RwLock};
use tempfile::tempdir;

/// A registry persistence failure after the thought was written must not turn
/// the append into an error (clients would retry and duplicate the thought).
#[test]
fn test_append_succeeds_when_registry_persist_fails_after_canonical_write() {
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key(dir.path(), "fail").unwrap();
    chain.append("a", ThoughtType::Finding, "first").unwrap();
    chain.flush().unwrap();

    let registry = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".agents.json"))
        .expect("agent registry file");
    std::fs::remove_file(&registry).unwrap();
    std::fs::create_dir(&registry).unwrap();

    chain
        .append("a", ThoughtType::Finding, "second")
        .expect("derived registry failure must not fail a persisted append");
    assert_eq!(chain.thoughts().len(), 2);
    chain.flush().unwrap();
    std::fs::remove_dir(&registry).unwrap();
    drop(chain);

    let reopened = MentisDb::open_with_key(dir.path(), "fail").unwrap();
    assert_eq!(reopened.thoughts().len(), 2, "exactly one copy persisted");
}

/// When a queued derived write fails, the writes that were not yet attempted
/// are kept and flushed by the next call instead of being dropped.
#[test]
fn test_failed_derived_flush_requeues_unprocessed_writes() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = mentisdb::search::EmbeddingProvider::metadata(&provider).clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "requeue").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    let wal = sidecar_wal_path(&chain.vector_sidecar_path(&metadata).unwrap());

    for text in ["one", "two"] {
        chain.defer_next_derived_flush();
        chain.append("a", ThoughtType::Finding, text).unwrap();
    }
    // Sabotage the first queued WAL write: a directory cannot be appended to.
    std::fs::create_dir(&wal).unwrap();
    assert!(chain.flush_pending_derived().is_err());
    std::fs::remove_dir(&wal).unwrap();

    chain.flush_pending_derived().unwrap();
    assert!(
        wal.exists(),
        "the second WAL record survived the failed flush and was written"
    );
}

/// Concurrent flushes under a shared lock serialize and all queued WAL
/// records reach disk before any caller returns.
#[test]
fn test_concurrent_flushes_do_not_lose_or_reorder_wal_records() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = mentisdb::search::EmbeddingProvider::metadata(&provider).clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "concurrent").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    let shared = Arc::new(RwLock::new(chain));

    for round in 0..10 {
        {
            let mut guard = shared.write().unwrap();
            guard.defer_next_derived_flush();
            guard
                .append("a", ThoughtType::Finding, &format!("round {round}"))
                .unwrap();
        }
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || shared.read().unwrap().flush_pending_derived().unwrap())
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }
    drop(shared);

    let reopened = MentisDb::open_with_key(dir.path(), "concurrent").unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar loads (WAL digest chain intact)");
    assert_eq!(sidecar.entries.len(), 10);
}
