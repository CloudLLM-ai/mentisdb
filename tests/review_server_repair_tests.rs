//! Derived-state adoption on the request path runs off the async workers and
//! still completes without a write request.
#![cfg(feature = "server")]

use mentisdb::search::{EmbeddingProvider, LocalTextEmbeddingProvider};
use mentisdb::server::{MentisDbService, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Reads alone must adopt a finished background rebuild (via the blocking
/// pool) and stop reporting pending work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_reads_adopt_finished_rebuild_in_background() {
    let dir = tempdir().unwrap();
    {
        let mut chain = MentisDb::open_with_key(dir.path(), "repair").unwrap();
        for i in 0..5 {
            chain
                .append("a", ThoughtType::Finding, &format!("thought {i}"))
                .unwrap();
        }
        chain.flush().unwrap();
    }
    let service = MentisDbService::new(MentisDbServiceConfig::new(
        dir.path().to_path_buf(),
        "default-chain",
        StorageAdapterKind::Binary,
    ));
    let handle = service.existing_chain_handle("repair").await.unwrap();
    let metadata = LocalTextEmbeddingProvider::new().metadata().clone();
    handle
        .write()
        .await
        .schedule_vector_sidecar_rebuild(metadata, || Ok(LocalTextEmbeddingProvider::new()))
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // Each read request only detects work and hands it to spawn_blocking.
        let _ = service.existing_chain_handle("repair").await.unwrap();
        if !handle.read().await.has_pending_sidecar_rebuilds() {
            break;
        }
        assert!(Instant::now() < deadline, "rebuild never adopted by reads");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
