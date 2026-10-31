//! Adversarial crash-recovery tests for the invariants in WHITEPAPER 4.4:
//! a chain file cut at any byte of its last record, and derived state (vector
//! sidecar) that is newer than or different from the chain it sits beside.

use mentisdb::search::{EmbeddingProvider, LocalTextEmbeddingProvider, VectorSidecarFreshness};
use mentisdb::{MentisDb, ThoughtType};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn chain_file(chain: &MentisDb) -> PathBuf {
    PathBuf::from(chain.storage_location())
}

/// Build a chain of `count` thoughts, flush it and return its file bytes.
fn build_chain(dir: &Path, key: &str, count: usize) -> (PathBuf, Vec<u8>) {
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    for i in 0..count {
        chain
            .append(
                "agent",
                ThoughtType::Finding,
                &format!("thought number {i}"),
            )
            .unwrap();
    }
    chain.flush().unwrap();
    let path = chain_file(&chain);
    drop(chain);
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

/// Byte offsets at which each length-prefixed record starts.
fn record_starts(bytes: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() {
        starts.push(at);
        let len = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
        at += 8 + len;
    }
    starts
}

/// Cut the final record at every possible byte. Every cut must open, keep
/// exactly the complete records, accept a new append, and reopen consistent
/// with the appended thought hash-linked to the surviving head.
#[test]
fn test_last_record_truncated_at_every_byte_boundary_recovers() {
    let source = tempdir().unwrap();
    let (_, bytes) = build_chain(source.path(), "cut", 3);
    let starts = record_starts(&bytes);
    assert_eq!(starts.len(), 3);
    let last = starts[2];

    for cut in last..=bytes.len() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(
            chain_file_name_for(source.path())
                .file_name()
                .expect("chain file name"),
        );
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let expected = if cut == bytes.len() { 3 } else { 2 };

        let mut chain = MentisDb::open_with_key(dir.path(), "cut").unwrap_or_else(|error| {
            panic!(
                "cut at {cut} of {} (last record starts at {last}) failed to open: {error}",
                bytes.len()
            )
        });
        assert_eq!(chain.thoughts().len(), expected, "cut at {cut}");
        assert!(chain.verify_integrity(), "cut at {cut}");
        chain
            .append("agent", ThoughtType::Finding, "after recovery")
            .unwrap();
        chain.flush().unwrap();
        drop(chain);

        let reopened = MentisDb::open_with_key(dir.path(), "cut").unwrap();
        assert_eq!(reopened.thoughts().len(), expected + 1, "cut at {cut}");
        assert!(reopened.verify_integrity(), "cut at {cut}");
        assert_eq!(
            reopened.thoughts()[expected].prev_hash,
            reopened.thoughts()[expected - 1].hash,
            "cut at {cut}"
        );
    }
}

fn chain_file_name_for(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "tcbin"))
        .expect("chain file in source directory")
}

/// A sidecar covering more thoughts than the chain now holds (chain restored
/// from an older backup while the sidecar was kept) is stale and must be
/// rebuilt, not trusted.
#[test]
fn test_sidecar_newer_than_chain_is_rebuilt_not_trusted() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = provider.metadata().clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "newer").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    for i in 0..5 {
        chain
            .append(
                "agent",
                ThoughtType::Finding,
                &format!("thought number {i}"),
            )
            .unwrap();
    }
    chain.flush().unwrap();
    let path = chain_file(&chain);
    drop(chain);

    let bytes = std::fs::read(&path).unwrap();
    let starts = record_starts(&bytes);
    std::fs::write(&path, &bytes[..starts[3]]).unwrap(); // keep 3 of 5

    let mut chain = MentisDb::open_with_key(dir.path(), "newer").unwrap();
    assert_eq!(chain.thoughts().len(), 3);
    let sidecar = chain
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    assert_eq!(sidecar.entries.len(), 3, "rebuilt to the restored chain");
    assert!(matches!(
        chain.vector_sidecar_freshness(&sidecar, &metadata).unwrap(),
        VectorSidecarFreshness::Fresh
    ));
}

/// A different chain with the same thought count under the same key must not
/// reuse a sidecar built for the old one: the head hash differs.
#[test]
fn test_sidecar_with_equal_count_but_different_head_is_rebuilt() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = provider.metadata().clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "swap").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    for i in 0..3 {
        chain
            .append("agent", ThoughtType::Finding, &format!("original {i}"))
            .unwrap();
    }
    chain.flush().unwrap();
    let path = chain_file(&chain);
    drop(chain);

    let other = tempdir().unwrap();
    let (other_path, other_bytes) = build_chain(other.path(), "swap", 3);
    assert_ne!(std::fs::read(&path).unwrap(), other_bytes);
    drop(other_path);
    std::fs::write(&path, other_bytes).unwrap();

    let mut chain = MentisDb::open_with_key(dir.path(), "swap").unwrap();
    let expected_ids: Vec<String> = chain.thoughts().iter().map(|t| t.id.to_string()).collect();
    let sidecar = chain
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    let sidecar_ids: Vec<String> = sidecar
        .entries
        .iter()
        .map(|entry| entry.thought_id.to_string())
        .collect();
    assert_eq!(sidecar_ids, expected_ids, "vectors belong to the new chain");
    assert!(matches!(
        chain.vector_sidecar_freshness(&sidecar, &metadata).unwrap(),
        VectorSidecarFreshness::Fresh
    ));
}

/// The narrowed torn-tail rule must still refuse a *corrupt* final length
/// prefix: when the bytes after it are one intact record, they are real data
/// and must not be truncated as a tail.
#[test]
fn test_inflated_final_length_prefix_over_intact_record_is_not_truncated() {
    let dir = tempdir().unwrap();
    let (path, mut bytes) = build_chain(dir.path(), "inflated", 3);
    let last = record_starts(&bytes)[2];
    let real = u64::from_le_bytes(bytes[last..last + 8].try_into().unwrap());
    bytes[last..last + 8].copy_from_slice(&(real + 50).to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();

    assert!(MentisDb::open_with_key(dir.path(), "inflated").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "file left untouched");
}
