//! Regression test: a finished background HNSW build is adopted (with the
//! appended delta upserted) instead of discarded under continuous appends.

use mentisdb::search::{LocalTextEmbeddingProvider, VectorBackendKind};
use mentisdb::{MentisDb, ThoughtType};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Appends racing a background HNSW build must still end with the HNSW
/// backend active and covering every indexed thought.
#[test]
fn test_hnsw_upgrade_survives_appends_during_build() {
    std::env::set_var("MENTISDB_HNSW_THRESHOLD", "20");
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key(dir.path(), "hnsw").unwrap();
    for i in 0..300 {
        chain
            .append(
                "a",
                ThoughtType::Finding,
                &format!("seed thought {i} about topic {}", i % 17),
            )
            .unwrap();
    }
    chain
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    // Keep appending while the graph builds in the background.
    for i in 0..20 {
        chain
            .append("a", ThoughtType::Finding, &format!("live append {i}"))
            .unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        chain.poll_vector_backend_upgrades();
        let status = chain
            .managed_vector_sidecar_statuses()
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        if status.backend_kind == Some(VectorBackendKind::Hnsw) && !status.backend_building {
            assert_eq!(status.indexed_thought_count, Some(320));
            break;
        }
        assert!(Instant::now() < deadline, "HNSW upgrade never adopted");
        std::thread::sleep(Duration::from_millis(20));
    }
}
