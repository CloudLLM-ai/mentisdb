//! Regression tests for per-append derived-state costs: the live vector
//! sidecar is reused (not cloned) and the implicit-edge overlay file is
//! persisted in amortized fashion.

use mentisdb::search::{ImplicitEdgeOverlay, LocalTextEmbeddingProvider, VectorSidecarFreshness};
use mentisdb::{MentisDb, ThoughtType};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn overlay_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("auto_edges"))
        })
}

fn edges_on_disk(dir: &Path) -> usize {
    overlay_file(dir)
        .and_then(|path| ImplicitEdgeOverlay::load_from_path(&path).ok())
        .map(|overlay| overlay.edges.values().map(Vec::len).sum())
        .unwrap_or(0)
}

/// The overlay file is rewritten every few appends rather than on each one,
/// is current after an explicit flush and after drop, and the sidecar stays
/// fresh and complete across the same appends.
#[test]
fn test_overlay_persistence_is_amortized_but_flushed_on_flush_and_drop() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = mentisdb::search::EmbeddingProvider::metadata(&provider).clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "overlay").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    for _ in 0..10 {
        chain
            .append("a", ThoughtType::Finding, "identical sentence about rust")
            .unwrap();
    }
    let live_edges = chain.implicit_edge_count();
    assert!(live_edges > 0, "identical thoughts must produce edges");
    assert!(
        edges_on_disk(dir.path()) < live_edges,
        "overlay file must not be rewritten on every append"
    );

    chain.flush().unwrap();
    assert_eq!(edges_on_disk(dir.path()), live_edges);

    chain
        .append("a", ThoughtType::Finding, "identical sentence about rust")
        .unwrap();
    let after_more = chain.implicit_edge_count();
    assert!(after_more > live_edges);
    drop(chain);
    assert_eq!(edges_on_disk(dir.path()), after_more);

    let reopened = MentisDb::open_with_key(dir.path(), "overlay").unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), 11);
    assert!(matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, &metadata)
            .unwrap(),
        VectorSidecarFreshness::Fresh
    ));
}
