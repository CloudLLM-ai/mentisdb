//! Sidecar WAL compaction runs off the chain lock.
//!
//! Folding the WAL into the JSON snapshot serializes and hashes the whole
//! corpus. These tests pin that the append path never does that work (it only
//! queues a request), that the background compaction produces a snapshot every
//! later append and load agrees with, and that every crash point of the
//! two-file swap recovers. Nothing here depends on wall-clock time: the
//! "off the lock" claim is checked by counting full-corpus digests on the
//! appending thread, and interleavings are forced through the stage hook.

use chrono::Utc;
use mentisdb::search::{
    append_sidecar_wal_record, compact_sidecar_snapshot, compact_sidecar_snapshot_with_hook,
    full_sidecar_digests_on_this_thread, sidecar_wal_path, spawn_sidecar_compaction,
    wait_for_sidecar_compaction, EmbeddingMetadata, LocalTextEmbeddingProvider,
    SidecarCompactionStage, VectorSidecar, VectorSidecarEntry, VectorSidecarFreshness,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use uuid::Uuid;

fn metadata() -> EmbeddingMetadata {
    EmbeddingMetadata::new("compaction-model", 3, "v1")
}

fn entry(index: u64) -> VectorSidecarEntry {
    VectorSidecarEntry::new(
        Uuid::from_u128(index as u128 + 1),
        index,
        format!("hash-{index}"),
        vec![1.0, index as f32 / 7.0, -0.5],
    )
}

/// An empty snapshot on disk plus the live in-memory sidecar that keeps
/// appending to its WAL, the way a managed chain handle does.
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    live: VectorSidecar,
    next: u64,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let path = dir.path().join("chain.vectors.json");
        let live =
            VectorSidecar::build("demo", metadata(), 0, None, Utc::now(), Vec::new()).unwrap();
        live.save_to_path(&path).unwrap();
        Self {
            _dir: dir,
            path,
            live,
            next: 0,
        }
    }

    /// Append `count` entries: extend the live sidecar and write the WAL record.
    fn append(&mut self, count: usize) {
        for _ in 0..count {
            let index = self.next;
            self.next += 1;
            let record = self
                .live
                .extend_with_entry(
                    entry(index),
                    index as usize + 1,
                    Some(format!("hash-{index}")),
                )
                .unwrap();
            append_sidecar_wal_record(&sidecar_wal_path(&self.path), &record).unwrap();
        }
    }

    fn assert_loads_with(&self, count: usize) {
        let loaded = VectorSidecar::load_from_path(&self.path).unwrap();
        assert_eq!(loaded.entries.len(), count);
        for (position, loaded_entry) in loaded.entries.iter().enumerate() {
            assert_eq!(*loaded_entry, entry(position as u64));
        }
        assert_eq!(loaded.thought_count, count);
        assert_eq!(
            loaded.integrity.digest_hex, self.live.integrity.digest_hex,
            "loaded chain head equals the live handle's"
        );
    }
}

fn wal_exists(path: &Path) -> bool {
    sidecar_wal_path(path).exists()
}

/// A compaction folds the whole WAL into the snapshot and deletes it, and the
/// snapshot's own digest still verifies (it is the full-corpus digest).
#[test]
fn test_compaction_folds_the_wal_into_a_verifiable_snapshot() {
    let mut fixture = Fixture::new();
    fixture.append(5);
    let report = compact_sidecar_snapshot(&fixture.path, &Mutex::new(()))
        .unwrap()
        .expect("compacted");
    assert_eq!(report.entries, 5);
    assert_eq!(report.folded_records, 5);
    assert_eq!(report.retained_records, 0);
    assert!(!wal_exists(&fixture.path));

    let raw: VectorSidecar =
        serde_json::from_slice(&std::fs::read(&fixture.path).unwrap()).unwrap();
    raw.verify_integrity().expect("full-corpus digest verifies");
    assert!(raw.wal_base_digest_hex.is_some());
    fixture.assert_loads_with(5);
}

/// The live handle never rebases: records it appends after a compaction chain
/// from its incremental digest and still replay against the compacted
/// snapshot. (Before the fix the handle adopted the full-corpus digest under the
/// write lock; now the snapshot records where the chain continues.)
#[test]
fn test_appends_after_compaction_replay_against_the_compacted_snapshot() {
    let mut fixture = Fixture::new();
    fixture.append(4);
    compact_sidecar_snapshot(&fixture.path, &Mutex::new(())).unwrap();
    fixture.append(3);
    fixture.assert_loads_with(7);
    compact_sidecar_snapshot(&fixture.path, &Mutex::new(())).unwrap();
    fixture.append(1);
    fixture.assert_loads_with(8);
}

/// Records appended while the compaction is hashing (after it copied the WAL)
/// are kept in the shortened WAL, not folded and not lost.
#[test]
fn test_records_appended_during_compaction_are_retained() {
    let mut fixture = Fixture::new();
    fixture.append(3);
    let mut hook_fixture = Some(&mut fixture);
    let path = hook_fixture.as_ref().unwrap().path.clone();
    let report = compact_sidecar_snapshot_with_hook(&path, &Mutex::new(()), &mut |stage| {
        if stage == SidecarCompactionStage::SnapshotWritten {
            hook_fixture.take().unwrap().append(2);
        }
        Ok(())
    })
    .unwrap()
    .expect("compacted");
    assert_eq!(report.folded_records, 3);
    assert_eq!(report.retained_records, 2);
    assert!(wal_exists(&fixture.path), "the retained tail is a WAL");
    fixture.assert_loads_with(5);
}

/// A crash after the new snapshot is written but before it is swapped in
/// leaves the old snapshot and WAL untouched.
#[test]
fn test_crash_before_the_snapshot_swap_keeps_the_old_files() {
    let mut fixture = Fixture::new();
    fixture.append(6);
    let error =
        compact_sidecar_snapshot_with_hook(
            &fixture.path,
            &Mutex::new(()),
            &mut |stage| match stage {
                SidecarCompactionStage::SnapshotWritten => Err(io::Error::other("crash")),
                _ => Ok(()),
            },
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "crash");
    fixture.assert_loads_with(6);
    // The leftover temporary file does not get in the way of the next run.
    let report = compact_sidecar_snapshot(&fixture.path, &Mutex::new(()))
        .unwrap()
        .expect("compacted");
    assert_eq!(report.folded_records, 6);
    fixture.assert_loads_with(6);
}

/// A crash between the snapshot swap and the WAL swap leaves a snapshot that
/// already holds records the old WAL still lists, plus records appended
/// meanwhile. Replay skips the covered ones and applies the rest.
#[test]
fn test_crash_between_the_snapshot_and_wal_swaps_recovers() {
    let mut fixture = Fixture::new();
    fixture.append(5);
    let path = fixture.path.clone();
    let mut hook_fixture = Some(&mut fixture);
    let error =
        compact_sidecar_snapshot_with_hook(&path, &Mutex::new(()), &mut |stage| match stage {
            SidecarCompactionStage::SnapshotWritten => {
                hook_fixture.take().unwrap().append(2);
                Ok(())
            }
            SidecarCompactionStage::SnapshotSwapped => Err(io::Error::other("crash")),
        })
        .unwrap_err();
    assert_eq!(error.to_string(), "crash");
    assert!(wal_exists(&fixture.path), "the long WAL is still there");
    fixture.assert_loads_with(7);

    // And the handle keeps appending after the recovery point.
    fixture.append(2);
    fixture.assert_loads_with(9);
}

/// The same crash point with nothing appended meanwhile: every WAL record is
/// covered by the snapshot, so replay skips all of them.
#[test]
fn test_crash_after_the_snapshot_swap_with_a_fully_covered_wal_recovers() {
    let mut fixture = Fixture::new();
    fixture.append(4);
    compact_sidecar_snapshot_with_hook(&fixture.path, &Mutex::new(()), &mut |stage| match stage {
        SidecarCompactionStage::SnapshotSwapped => Err(io::Error::other("crash")),
        _ => Ok(()),
    })
    .unwrap_err();
    fixture.assert_loads_with(4);
    fixture.append(1);
    fixture.assert_loads_with(5);
}

/// A WAL that belongs to a different sidecar is rejected, not skipped: the
/// leniency for covered records must not let an unrelated log through.
#[test]
fn test_wal_from_another_sidecar_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.append(3);
    // A log written for a different chain key, hence a different digest chain.
    let mut different =
        VectorSidecar::build("other", metadata(), 0, None, Utc::now(), Vec::new()).unwrap();
    for index in 0..3_u64 {
        let record = different
            .extend_with_entry(
                VectorSidecarEntry::new(
                    Uuid::from_u128(1000 + index as u128),
                    index,
                    format!("other-{index}"),
                    vec![0.0, 1.0, 0.0],
                ),
                index as usize + 1,
                Some(format!("other-{index}")),
            )
            .unwrap();
        if index == 0 {
            std::fs::remove_file(sidecar_wal_path(&fixture.path)).unwrap();
        }
        append_sidecar_wal_record(&sidecar_wal_path(&fixture.path), &record).unwrap();
    }
    assert!(VectorSidecar::load_from_path(&fixture.path).is_err());
}

/// A WAL that ends inside a record is not compacted (the torn bytes would be
/// kept in front of the next append); the files stay as they were.
#[test]
fn test_compaction_refuses_a_torn_wal_and_changes_nothing() {
    let mut fixture = Fixture::new();
    fixture.append(3);
    let wal = sidecar_wal_path(&fixture.path);
    let mut bytes = std::fs::read(&wal).unwrap();
    bytes.extend_from_slice(&[1, 0]); // half a length prefix
    std::fs::write(&wal, &bytes).unwrap();
    assert!(compact_sidecar_snapshot(&fixture.path, &Mutex::new(())).is_err());
    assert_eq!(std::fs::read(&wal).unwrap(), bytes);
    assert!(!VectorSidecar::load_from_path(&fixture.path)
        .unwrap()
        .entries
        .is_empty());
}

/// Another writer replacing the snapshot while a compaction runs (a full
/// rebuild adopted) cancels the compaction instead of overwriting the newer
/// file.
#[test]
fn test_compaction_yields_when_the_snapshot_is_replaced_underneath() {
    let mut fixture = Fixture::new();
    fixture.append(3);
    let path = fixture.path.clone();
    let replacement = VectorSidecar::build(
        "demo",
        metadata(),
        1,
        Some("hash-0".to_owned()),
        Utc::now(),
        vec![entry(0)],
    )
    .unwrap();
    let outcome = compact_sidecar_snapshot_with_hook(&path, &Mutex::new(()), &mut |stage| {
        if stage == SidecarCompactionStage::SnapshotWritten {
            // A file of a different length: the stamp check cannot miss it.
            replacement.save_to_path(&path).unwrap();
        }
        Ok(())
    })
    .unwrap();
    assert!(outcome.is_none());
    let loaded = VectorSidecar::load_from_path(&path).unwrap();
    assert_eq!(loaded.entries.len(), 1, "the newer snapshot survived");
    assert!(
        !std::fs::read_dir(path.parent().unwrap()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".compact.tmp")),
        "no temporary file is left behind"
    );
}

/// With no snapshot there is nothing to compact.
#[test]
fn test_compaction_without_a_snapshot_is_a_no_op() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("missing.json");
    assert!(compact_sidecar_snapshot(&path, &Mutex::new(()))
        .unwrap()
        .is_none());
}

/// A second request for a snapshot that is already being compacted is dropped
/// rather than stacking threads. The first compaction is held at the I/O lock
/// the test owns, so "already running" is deterministic.
#[test]
fn test_spawned_compaction_is_single_flight_per_snapshot() {
    let mut fixture = Fixture::new();
    fixture.append(3);
    let io_lock = Arc::new(Mutex::new(()));
    let held = io_lock.lock().unwrap();
    assert!(spawn_sidecar_compaction(
        fixture.path.clone(),
        Arc::clone(&io_lock)
    ));
    assert!(
        !spawn_sidecar_compaction(fixture.path.clone(), Arc::clone(&io_lock)),
        "second request while the first is in flight"
    );
    drop(held);
    wait_for_sidecar_compaction(&fixture.path);
    assert!(!wal_exists(&fixture.path));
    fixture.assert_loads_with(3);
    assert!(spawn_sidecar_compaction(
        fixture.path.clone(),
        Arc::clone(&io_lock)
    ));
    wait_for_sidecar_compaction(&fixture.path);
}

/// Regression: a compaction used to run `refresh_snapshot_integrity` (a full
/// JSON serialization and SHA-256 of every vector) and clone the whole sidecar
/// inside the append path, i.e. under the chain's write lock. The managed
/// append path must now queue a request only: the calling thread computes no
/// full-corpus digest across a compaction threshold, and the background
/// compaction leaves a sidecar a later reopen finds complete and fresh.
#[test]
fn test_append_path_never_hashes_the_corpus_across_a_compaction() {
    let dir = tempdir().unwrap();
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = mentisdb::search::EmbeddingProvider::metadata(&provider).clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "compact").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    let snapshot_path = chain.vector_sidecar_path(&metadata).unwrap();

    let total = 80; // two compactions at the minimum threshold of 32
    let digests_before = full_sidecar_digests_on_this_thread();
    for n in 0..total {
        chain.defer_next_derived_flush();
        chain.defer_next_embedding();
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Finding, format!("compaction entry {n}")),
            )
            .unwrap();
        let outcomes: Vec<_> = chain
            .take_embed_tickets()
            .into_iter()
            .map(|ticket| ticket.embed())
            .collect();
        chain.commit_embeddings(outcomes);
        chain.flush_pending_derived().unwrap();
    }
    assert_eq!(
        full_sidecar_digests_on_this_thread(),
        digests_before,
        "the append, commit and flush path hashed the corpus"
    );
    wait_for_sidecar_compaction(&snapshot_path);
    chain.flush().unwrap();
    drop(chain);
    wait_for_sidecar_compaction(&snapshot_path);

    let reopened = MentisDb::open_with_key(dir.path(), "compact").unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), total);
    assert!(matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, &metadata)
            .unwrap(),
        VectorSidecarFreshness::Fresh
    ));
    let raw: VectorSidecar =
        serde_json::from_slice(&std::fs::read(&snapshot_path).unwrap()).unwrap();
    assert!(
        raw.wal_base_digest_hex.is_some() && raw.entries.len() >= 32,
        "at least one compaction folded the WAL into the snapshot"
    );
}

/// Regression: a load that read the old snapshot and then found the WAL gone
/// (a compaction swapped the snapshot and deleted the WAL in between) returned
/// the old snapshot alone, silently dropping every record the WAL held, so a
/// reopen during a compaction saw a stale sidecar and rebuilt it. The load now
/// notices the snapshot changed and reads again.
#[test]
fn test_load_during_a_compaction_never_loses_wal_records() {
    let mut fixture = Fixture::new();
    fixture.append(6);
    let path = fixture.path.clone();
    let mut compacted = false;
    let loaded = VectorSidecar::load_from_path_with_hook(&path, &mut || {
        if !compacted {
            compacted = true;
            compact_sidecar_snapshot(&path, &Mutex::new(()))
                .unwrap()
                .expect("compacted");
        }
    })
    .unwrap();
    assert_eq!(loaded.entries.len(), 6);
    assert!(!wal_exists(&path), "the compaction did run mid-load");
}
