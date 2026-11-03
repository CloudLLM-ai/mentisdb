#![no_main]
//! Fuzzes vector sidecar loading and background compaction with arbitrary WAL
//! bytes beside a valid snapshot.
//!
//! The WAL is read from disk and may be torn by a crash, truncated, reordered
//! or hand-edited. Invariants: loading never panics; a sidecar that loads is
//! well formed (every row has the sidecar's dimension and finite components,
//! rows are ordered by `thought_index`); and compaction either refuses or
//! produces a snapshot that loads to exactly the same rows with the same WAL
//! chain head, so folding the WAL never changes what a load returns.

use chrono::Utc;
use libfuzzer_sys::fuzz_target;
use mentisdb::search::{
    append_sidecar_wal_record, compact_sidecar_snapshot, sidecar_wal_path, EmbeddingMetadata,
    VectorSidecar, VectorSidecarEntry,
};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

/// A valid empty snapshot and the bytes of a valid two-record WAL for it.
struct Fixture {
    dir: PathBuf,
    snapshot: VectorSidecar,
    good_wal: Vec<u8>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("mentisdb-fuzz-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.json");
        let metadata = EmbeddingMetadata::new("fuzz", 3, "v1");
        let snapshot =
            VectorSidecar::build("fuzz", metadata, 0, None, Utc::now(), Vec::new()).unwrap();
        snapshot.save_to_path(&path).unwrap();
        let mut live = snapshot.clone();
        for index in 0..2_u64 {
            let record = live
                .extend_with_entry(
                    VectorSidecarEntry::new(
                        Uuid::from_u128(index as u128 + 1),
                        index,
                        format!("hash-{index}"),
                        vec![1.0, index as f32, 0.5],
                    ),
                    index as usize + 1,
                    Some(format!("hash-{index}")),
                )
                .unwrap();
            append_sidecar_wal_record(&sidecar_wal_path(&path), &record).unwrap();
        }
        let good_wal = std::fs::read(sidecar_wal_path(&path)).unwrap();
        Fixture {
            dir,
            snapshot,
            good_wal,
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let fixture = fixture();
    let path = fixture.dir.join("case.json");
    fixture.snapshot.save_to_path(&path).unwrap();
    let wal = match data.split_first() {
        // Bit 0 chooses between raw bytes and valid records with a hostile tail.
        Some((first, rest)) if first & 1 == 1 => [fixture.good_wal.as_slice(), rest].concat(),
        Some((_, rest)) => rest.to_vec(),
        None => Vec::new(),
    };
    std::fs::write(sidecar_wal_path(&path), &wal).unwrap();

    let Ok(loaded) = VectorSidecar::load_from_path(&path) else {
        return;
    };
    let mut previous = None;
    for entry in &loaded.entries {
        assert_eq!(entry.vector.len(), loaded.metadata.dimension);
        assert!(entry.vector.iter().all(|value| value.is_finite()));
        assert!(previous.is_none_or(|index| entry.thought_index >= index));
        previous = Some(entry.thought_index);
    }
    assert_eq!(loaded.integrity.entry_count, loaded.entries.len());

    if let Ok(Some(_)) = compact_sidecar_snapshot(&path, &Mutex::new(())) {
        let again = VectorSidecar::load_from_path(&path).expect("a compacted sidecar loads");
        assert_eq!(again.entries, loaded.entries);
        assert_eq!(again.integrity.digest_hex, loaded.integrity.digest_hex);
        assert_eq!(again.thought_count, loaded.thought_count);
    }
});
