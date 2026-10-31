//! A chain file is untrusted input (restores, imports, copied directories).
//! A length prefix inside a record that claims far more bytes than the file
//! holds must produce an error, not a multi-terabyte allocation that aborts
//! the process.

use mentisdb::{MentisDb, ThoughtType};
use std::path::PathBuf;
use tempfile::tempdir;

const MARKER: &str = "MARKER-CONTENT-FOR-LENGTH-REWRITE";

/// Rewrite the varint length of the content string inside the only record to
/// claim `claimed` bytes, fixing up the record's outer length prefix so the
/// framing stays valid.
fn forge_string_length(path: &PathBuf, claimed: u64) {
    let bytes = std::fs::read(path).unwrap();
    let at = bytes
        .windows(MARKER.len())
        .position(|window| window == MARKER.as_bytes())
        .expect("marker present");
    assert_eq!(
        bytes[at - 1] as usize,
        MARKER.len(),
        "one-byte varint length"
    );
    let mut forged = Vec::new();
    forged.extend_from_slice(&bytes[..at - 1]);
    forged.push(253); // bincode varint marker for a following u64
    forged.extend_from_slice(&claimed.to_le_bytes());
    forged.extend_from_slice(&bytes[at..]);
    let payload_len = (forged.len() - 8) as u64;
    forged[..8].copy_from_slice(&payload_len.to_le_bytes());
    std::fs::write(path, forged).unwrap();
}

/// Opening a chain whose content length prefix claims a terabyte is refused
/// with an error instead of attempting the allocation.
#[test]
fn test_huge_string_length_prefix_is_rejected_not_allocated() {
    let dir = tempdir().unwrap();
    let path = {
        let mut chain = MentisDb::open_with_key(dir.path(), "bomb").unwrap();
        chain.append("a", ThoughtType::Finding, MARKER).unwrap();
        chain.flush().unwrap();
        PathBuf::from(chain.storage_location())
    };
    forge_string_length(&path, 1 << 50);
    let opened = MentisDb::open_with_key(dir.path(), "bomb");
    assert!(opened.is_err(), "a forged length must not open as a chain");
}

/// A persisted HNSW graph is read through a streaming decoder, which sizes its
/// buffers from the claimed lengths. A forged length must fail the load, not
/// abort the process with an impossible allocation.
#[test]
fn test_hnsw_graph_with_forged_string_length_fails_to_load() {
    use mentisdb::search::hnsw_backend::HnswBackend;
    use mentisdb::search::{EmbeddingMetadata, VectorDocument};
    let dir = tempdir().unwrap();
    let path = dir.path().join("hnsw.graph.bin");
    let metadata = EmbeddingMetadata::new("toy-model", 2, "v1");
    let backend = HnswBackend::from_documents(
        metadata.clone(),
        vec![VectorDocument::new("alpha", vec![1.0, 0.0])],
    )
    .unwrap();
    backend.persist_to_path(&path).unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        bytes[0] as usize,
        "toy-model".len(),
        "model id leads the file"
    );
    let mut forged = vec![253];
    forged.extend_from_slice(&(1_u64 << 50).to_le_bytes());
    forged.extend_from_slice(&bytes[1..]);
    std::fs::write(&path, forged).unwrap();

    let error =
        HnswBackend::load_from_path(&path, &metadata).expect_err("forged graph must not load");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
