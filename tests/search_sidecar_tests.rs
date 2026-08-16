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
