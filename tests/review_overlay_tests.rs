//! Regression tests for implicit-edge overlay crash recovery.

use mentisdb::search::{EmbeddingProvider, ImplicitEdgeOverlay, LocalTextEmbeddingProvider};
use mentisdb::{MentisDb, ThoughtType};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const SENTENCES: [&str; 7] = [
    "identical sentence about rust ownership",
    "identical sentence about rust ownership",
    "identical sentence about rust ownership today",
    "identical sentence about rust ownership",
    "identical sentence about rust ownership again",
    "identical sentence about rust ownership",
    "identical sentence about rust ownership now",
];

fn overlay_path(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name().unwrap().to_string_lossy().ends_with(".bin")
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains("auto_edges")
        })
        .expect("overlay file exists")
}

/// Order-insensitive view of the edges: id -> sorted "neighbor:score" list.
fn normalized(overlay: &ImplicitEdgeOverlay) -> BTreeMap<String, Vec<String>> {
    overlay
        .edges
        .iter()
        .map(|(id, neighbors)| {
            let mut entries: Vec<String> = neighbors
                .iter()
                .map(|n| format!("{}:{:.3}", n.thought_id, n.cosine_score))
                .collect();
            entries.sort();
            (id.to_string(), entries)
        })
        .collect()
}

fn populated_chain(dir: &Path, key: &str) -> MentisDb {
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    chain
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    for sentence in SENTENCES {
        chain.append("a", ThoughtType::Finding, sentence).unwrap();
    }
    chain
}

/// After a simulated crash (no flush/drop) with fewer than 32 appends since
/// the last overlay save, reopening must catch the overlay up so its edges
/// equal a fresh full rebuild.
#[test]
fn test_overlay_catches_up_after_crash_before_persist_interval() {
    let dir = tempdir().unwrap();
    let metadata = LocalTextEmbeddingProvider::new().metadata().clone();
    let chain = populated_chain(dir.path(), "crash");
    assert!(
        chain.implicit_edge_count() > 0,
        "fixture must produce edges"
    );
    std::mem::forget(chain);

    let mut reopened = MentisDb::open_with_key(dir.path(), "crash").unwrap();
    reopened
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar");
    assert_eq!(sidecar.entries.len(), SENTENCES.len());
    reopened.flush().unwrap();
    let on_disk = ImplicitEdgeOverlay::load_from_path(&overlay_path(dir.path())).unwrap();
    assert_eq!(on_disk.covered_thought_count, Some(SENTENCES.len()));
    let fresh = ImplicitEdgeOverlay::build_from_sidecar(&sidecar, on_disk.k, on_disk.threshold);
    assert!(!fresh.edges.is_empty());
    assert_eq!(normalized(&on_disk), normalized(&fresh));
}

/// An overlay whose recorded head hash does not match the chain is rebuilt.
#[test]
fn test_overlay_with_mismatched_head_is_rebuilt() {
    let dir = tempdir().unwrap();
    let chain = populated_chain(dir.path(), "mismatch");
    chain.flush().unwrap();
    drop(chain);
    let path = overlay_path(dir.path());
    let mut overlay = ImplicitEdgeOverlay::load_from_path(&path).unwrap();
    overlay.edges.clear();
    overlay.set_coverage(SENTENCES.len() - 2, "bogus-hash");
    overlay.save_to_path(&path).unwrap();

    let mut reopened = MentisDb::open_with_key(dir.path(), "mismatch").unwrap();
    reopened
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    assert!(
        reopened.implicit_edge_count() > 0,
        "overlay must be rebuilt"
    );
}
