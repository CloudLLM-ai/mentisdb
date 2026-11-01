//! Regression tests for HNSW graph trust: a persisted graph is reused only when
//! it was built from exactly the documents (and embedding space) it is asked to
//! serve, not merely when the entry counts agree.

use mentisdb::search::hnsw_backend::{documents_digest, HnswBackend, GRAPH_HEADER_LEN};
use mentisdb::search::{EmbeddingMetadata, VectorDocument};
use std::io::ErrorKind;
use tempfile::tempdir;

fn meta() -> EmbeddingMetadata {
    EmbeddingMetadata::new("toy", 2, "v1")
}

fn docs(vectors: &[[f32; 2]]) -> Vec<VectorDocument> {
    vectors
        .iter()
        .enumerate()
        .map(|(i, v)| VectorDocument::new(format!("d{i}"), v.to_vec()))
        .collect()
}

fn digest_of(documents: &[VectorDocument]) -> [u8; 32] {
    documents_digest(
        &meta(),
        documents
            .iter()
            .map(|d| (d.document_id.as_str(), d.vector.as_slice())),
    )
}

/// A graph built from the same documents loads when the expected digest matches.
#[test]
fn test_matching_digest_loads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("g.bin");
    let documents = docs(&[[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]]);
    HnswBackend::from_documents(meta(), documents.clone())
        .unwrap()
        .persist_to_path(&path)
        .unwrap();
    HnswBackend::load_from_path_matching(&path, &meta(), &digest_of(&documents))
        .expect("same content must load");
}

/// Same entry count, same ids, different vectors: must be rejected.
#[test]
fn test_same_count_different_content_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("g.bin");
    let old = docs(&[[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0]]);
    let new = docs(&[[1.0, 0.0], [0.0, 1.0], [0.0, -1.0]]);
    HnswBackend::from_documents(meta(), old)
        .unwrap()
        .persist_to_path(&path)
        .unwrap();
    let error = HnswBackend::load_from_path_matching(&path, &meta(), &digest_of(&new))
        .expect_err("a same-count graph of other content must not load");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

/// The digest also binds the embedding space, so a model swap with identical
/// vectors is rejected even through the digest alone.
#[test]
fn test_digest_binds_model_and_dimension() {
    let documents = docs(&[[1.0, 0.0]]);
    let other = EmbeddingMetadata::new("other-model", 2, "v1");
    let a = digest_of(&documents);
    let b = documents_digest(
        &other,
        documents
            .iter()
            .map(|d| (d.document_id.as_str(), d.vector.as_slice())),
    );
    assert_ne!(a, b);
}

/// A file without the header (the pre-digest format) is refused, not trusted.
#[test]
fn test_legacy_headerless_graph_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("g.bin");
    HnswBackend::from_documents(meta(), docs(&[[1.0, 0.0]]))
        .unwrap()
        .persist_to_path(&path)
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[GRAPH_HEADER_LEN..]).unwrap();
    assert_eq!(
        HnswBackend::load_from_path(&path, &meta())
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidData
    );
}

/// A body altered after the header was written fails the integrity check.
#[test]
fn test_body_tamper_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("g.bin");
    HnswBackend::from_documents(meta(), docs(&[[1.0, 0.0], [0.0, 1.0]]))
        .unwrap()
        .persist_to_path(&path)
        .unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    // The exact-vector cache sits at the tail: flip the last float's bits.
    let n = bytes.len();
    bytes[n - 2] ^= 0x7f;
    std::fs::write(&path, bytes).unwrap();
    assert!(HnswBackend::load_from_path(&path, &meta()).is_err());
}

/// Every truncation of a valid file is an error, never a panic or a partial load.
#[test]
fn test_truncated_graph_never_loads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("g.bin");
    HnswBackend::from_documents(meta(), docs(&[[1.0, 0.0], [0.0, 1.0]]))
        .unwrap()
        .persist_to_path(&path)
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    for cut in 0..bytes.len() {
        std::fs::write(&path, &bytes[..cut]).unwrap();
        assert!(
            HnswBackend::load_from_path(&path, &meta()).is_err(),
            "truncation at {cut} must not load"
        );
    }
}
