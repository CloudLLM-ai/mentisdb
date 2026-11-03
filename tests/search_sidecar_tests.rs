use chrono::Utc;
use mentisdb::search::{
    append_sidecar_wal_record, sidecar_wal_path, EmbeddingMetadata, VectorSidecar,
    VectorSidecarEntry, VectorSidecarFreshness,
};
use std::fs;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn vector_sidecar_round_trips_with_integrity() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        2,
        Some("head-a".to_string()),
        Utc::now(),
        vec![
            VectorSidecarEntry::new(Uuid::new_v4(), 0, "hash-a", vec![1.0, 0.0]),
            VectorSidecarEntry::new(Uuid::new_v4(), 1, "hash-b", vec![0.0, 1.0]),
        ],
    )
    .unwrap();

    sidecar.save_to_path(&path).unwrap();
    let loaded = VectorSidecar::load_from_path(&path).unwrap();

    assert_eq!(loaded.chain_key, "mentisdb");
    assert_eq!(loaded.metadata.model_id, "local-model");
    assert_eq!(loaded.metadata.embedding_version, "v1");
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(loaded.integrity.entry_count, 2);
}

#[test]
fn vector_sidecar_detects_corruption() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(
            Uuid::new_v4(),
            0,
            "hash-a",
            vec![1.0, 0.0],
        )],
    )
    .unwrap();

    sidecar.save_to_path(&path).unwrap();
    let mut corrupted = fs::read_to_string(&path).unwrap();
    corrupted = corrupted.replace("\"hash-a\"", "\"hash-b\"");
    fs::write(&path, corrupted).unwrap();

    let error = VectorSidecar::load_from_path(&path).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("integrity"));
}

#[test]
fn vector_sidecar_freshness_detects_model_and_chain_drift() {
    let sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        2,
        Some("head-a".to_string()),
        Utc::now(),
        vec![
            VectorSidecarEntry::new(Uuid::new_v4(), 0, "hash-a", vec![1.0, 0.0]),
            VectorSidecarEntry::new(Uuid::new_v4(), 1, "hash-b", vec![0.0, 1.0]),
        ],
    )
    .unwrap();

    assert_eq!(
        sidecar.freshness(
            "mentisdb",
            2,
            Some("head-a"),
            &EmbeddingMetadata::new("local-model", 2, "v1"),
        ),
        VectorSidecarFreshness::Fresh
    );
    assert_eq!(
        sidecar.freshness(
            "mentisdb",
            2,
            Some("head-a"),
            &EmbeddingMetadata::new("local-model", 2, "v2"),
        ),
        VectorSidecarFreshness::EmbeddingVersionMismatch {
            expected: "v2".to_string(),
            actual: "v1".to_string(),
        }
    );
    assert_eq!(
        sidecar.freshness(
            "mentisdb",
            3,
            Some("head-b"),
            &EmbeddingMetadata::new("local-model", 2, "v1"),
        ),
        VectorSidecarFreshness::StaleThoughtCount {
            expected: 3,
            actual: 2,
        }
    );
}

#[test]
fn vector_sidecar_wal_replays_after_snapshot() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let id_a = Uuid::new_v4();
    let id_b = Uuid::new_v4();
    let mut sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(id_a, 0, "hash-a", vec![1.0, 0.0])],
    )
    .unwrap();
    sidecar.save_to_path(&path).unwrap();

    let record = sidecar
        .extend_with_entry(
            VectorSidecarEntry::new(id_b, 1, "hash-b", vec![0.0, 1.0]),
            2,
            Some("head-b".to_string()),
        )
        .unwrap();
    append_sidecar_wal_record(&sidecar_wal_path(&path), &record).unwrap();

    let loaded = VectorSidecar::load_from_path(&path).unwrap();
    assert_eq!(loaded.entries.len(), 2);
    assert_eq!(loaded.thought_count, 2);
    assert_eq!(loaded.head_hash.as_deref(), Some("head-b"));
    assert_eq!(loaded.entries[1].thought_id, id_b);
}

/// Verifies that compacting a sidecar rebases its in-memory WAL chain head onto
/// the snapshot digest, so appending after a compaction replays on the next load.
#[test]
fn vector_sidecar_append_after_compact_replays() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let mut sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(
            Uuid::new_v4(),
            0,
            "hash-a",
            vec![1.0, 0.0],
        )],
    )
    .unwrap();
    sidecar.save_to_path(&path).unwrap();

    let record = sidecar
        .extend_with_entry(
            VectorSidecarEntry::new(Uuid::new_v4(), 1, "hash-b", vec![0.0, 1.0]),
            2,
            Some("head-b".to_string()),
        )
        .unwrap();
    let wal = sidecar_wal_path(&path);
    append_sidecar_wal_record(&wal, &record).unwrap();

    sidecar.compact_to_path(&path).unwrap();
    assert!(!wal.exists());
    let compacted = VectorSidecar::load_from_path(&path).unwrap();
    assert_eq!(
        sidecar.integrity, compacted.integrity,
        "compaction must rebase the in-memory WAL head onto the snapshot digest"
    );

    let record = sidecar
        .extend_with_entry(
            VectorSidecarEntry::new(Uuid::new_v4(), 2, "hash-c", vec![0.5, 0.5]),
            3,
            Some("head-c".to_string()),
        )
        .unwrap();
    append_sidecar_wal_record(&wal, &record).unwrap();

    let loaded = VectorSidecar::load_from_path(&path)
        .expect("append after compact must replay against the compacted snapshot digest");
    assert_eq!(loaded.entries.len(), 3);
    assert_eq!(loaded.thought_count, 3);
}

/// Verifies that writing a full snapshot clears a stale sibling WAL, so a
/// from-scratch rebuild can never leave an older WAL to be replayed against a
/// newer snapshot.
#[test]
fn vector_sidecar_full_snapshot_clears_stale_wal() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let wal = sidecar_wal_path(&path);

    let mut first = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(
            Uuid::new_v4(),
            0,
            "hash-a",
            vec![1.0, 0.0],
        )],
    )
    .unwrap();
    first.save_to_path(&path).unwrap();
    let record = first
        .extend_with_entry(
            VectorSidecarEntry::new(Uuid::new_v4(), 1, "hash-b", vec![0.0, 1.0]),
            2,
            Some("head-b".to_string()),
        )
        .unwrap();
    append_sidecar_wal_record(&wal, &record).unwrap();
    assert!(wal.exists());

    let rebuilt = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(
            Uuid::new_v4(),
            0,
            "hash-c",
            vec![0.5, 0.5],
        )],
    )
    .unwrap();
    rebuilt.save_to_path(&path).unwrap();

    assert!(!wal.exists(), "a full snapshot must clear the stale WAL");
    let loaded = VectorSidecar::load_from_path(&path).unwrap();
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].thought_hash, "hash-c");
}

#[test]
fn vector_sidecar_compact_removes_wal() {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let mut sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        1,
        Some("head-a".to_string()),
        Utc::now(),
        vec![VectorSidecarEntry::new(
            Uuid::new_v4(),
            0,
            "hash-a",
            vec![1.0, 0.0],
        )],
    )
    .unwrap();
    sidecar.save_to_path(&path).unwrap();
    let record = sidecar
        .extend_with_entry(
            VectorSidecarEntry::new(Uuid::new_v4(), 1, "hash-b", vec![0.0, 1.0]),
            2,
            Some("head-b".to_string()),
        )
        .unwrap();
    let wal = sidecar_wal_path(&path);
    append_sidecar_wal_record(&wal, &record).unwrap();
    assert!(wal.exists());
    sidecar.compact_to_path(&path).unwrap();
    assert!(!wal.exists());
    let loaded = VectorSidecar::load_from_path(&path).unwrap();
    assert_eq!(loaded.entries.len(), 2);
    loaded.verify_integrity().unwrap();
}

/// A WAL whose records carry valid digests but a malformed row. The digest
/// chain is a checksum, not an authenticator, so replay must apply the same
/// row checks as a snapshot load. The record is crafted by extending a clone
/// whose metadata is altered, so its digest chains from the real sidecar.
fn load_with_crafted_wal_row(
    vector: Vec<f32>,
    index: u64,
    dimension: usize,
) -> std::io::Result<VectorSidecar> {
    let tempdir = TempDir::new().unwrap();
    let path = tempdir.path().join("chain.vectors.model.v1.json");
    let sidecar = VectorSidecar::build(
        "mentisdb",
        EmbeddingMetadata::new("local-model", 2, "v1"),
        0,
        None,
        Utc::now(),
        Vec::new(),
    )
    .unwrap();
    sidecar.save_to_path(&path).unwrap();
    let mut crafted = sidecar.clone();
    crafted.metadata.dimension = dimension;
    let record = crafted
        .extend_with_entry(
            VectorSidecarEntry::new(Uuid::new_v4(), index, "hash-x", vector),
            1,
            Some("hash-x".to_string()),
        )
        .unwrap();
    append_sidecar_wal_record(&sidecar_wal_path(&path), &record).unwrap();
    VectorSidecar::load_from_path(&path)
}

#[test]
fn vector_sidecar_wal_replay_rejects_a_row_of_the_wrong_dimension() {
    let error = load_with_crafted_wal_row(vec![1.0, 0.0, 0.0], 0, 3).unwrap_err();
    assert!(error.to_string().contains("dimension"), "{error}");
}

#[test]
fn vector_sidecar_wal_replay_rejects_a_non_finite_component() {
    let error = load_with_crafted_wal_row(vec![f32::NAN, 0.0], 0, 2).unwrap_err();
    assert!(error.to_string().contains("non-finite"), "{error}");
}

#[test]
fn vector_sidecar_wal_replay_accepts_a_well_formed_row() {
    let loaded = load_with_crafted_wal_row(vec![0.5, -0.5], 0, 2).unwrap();
    assert_eq!(loaded.entries.len(), 1);
}
