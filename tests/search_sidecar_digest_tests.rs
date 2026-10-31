//! The sidecar's full-corpus digest is persisted on disk, so its definition is
//! a compatibility contract: SHA-256 over the compact JSON of a fixed payload.
//! Streaming the serialization into the hasher must not change a single byte.

use chrono::{TimeZone, Utc};
use mentisdb::search::{
    wal_compact_threshold, EmbeddingMetadata, VectorSidecar, VectorSidecarEntry,
    VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Mirror of the persisted digest payload; field order is part of the format.
#[derive(Serialize)]
struct DigestPayload<'a> {
    schema_version: u32,
    chain_key: &'a str,
    metadata: &'a EmbeddingMetadata,
    thought_count: usize,
    head_hash: &'a Option<String>,
    generated_at: chrono::DateTime<Utc>,
    entries: &'a [VectorSidecarEntry],
}

/// The streamed digest equals SHA-256 of the buffered canonical JSON, so
/// sidecars written by earlier versions still verify.
#[test]
fn test_streamed_integrity_digest_matches_buffered_sha256() {
    let metadata = EmbeddingMetadata::new("pinned", 3, "v1");
    let entries: Vec<VectorSidecarEntry> = (0..5_u128)
        .map(|i| {
            VectorSidecarEntry::new(
                Uuid::from_u128(i + 1),
                i as u64,
                format!("hash-{i}"),
                vec![i as f32 / 7.0, -0.25, 1.5],
            )
        })
        .collect();
    let generated_at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
    let head = Some("hash-4".to_owned());
    let sidecar = VectorSidecar::build(
        "pinned-chain",
        metadata.clone(),
        5,
        head.clone(),
        generated_at,
        entries.clone(),
    )
    .unwrap();

    let canonical = serde_json::to_vec(&DigestPayload {
        schema_version: sidecar.schema_version,
        chain_key: "pinned-chain",
        metadata: &metadata,
        thought_count: 5,
        head_hash: &head,
        generated_at,
        entries: &entries,
    })
    .unwrap();
    let expected: String = Sha256::digest(canonical)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(sidecar.integrity.digest_hex, expected);
}

/// Small corpora keep the fixed threshold; large ones scale so the amortized
/// compaction cost per append stays constant instead of growing with the chain.
#[test]
fn test_wal_compaction_threshold_scales_with_the_corpus() {
    assert_eq!(
        wal_compact_threshold(0),
        VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD
    );
    assert_eq!(
        wal_compact_threshold(511),
        VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD
    );
    assert_eq!(wal_compact_threshold(10_000), 625);
    assert!(wal_compact_threshold(100_000) > wal_compact_threshold(10_000));
}
