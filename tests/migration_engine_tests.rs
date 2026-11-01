//! Tests for the journaled chain migration engine: every historical format
//! round-trips field by field, a kill at any step recovers cleanly, corrupt and
//! tampered chains are refused without being touched, and backups, rollback,
//! skip and retry behave.

use mentisdb::migration::fixtures::{
    expected_after_migration, semantic_chain, write_fixture, FixtureFormat,
};
use mentisdb::migration::{
    detect_migration, dry_run_chain, recover_chain, retry_chain, rollback_chain,
    run_chain_migration, skip_chain, verify_chain_file, Failpoint, JournalState, MigrationError,
    MigrationJournal, MigrationOptions, MigrationOutcome, MigrationPaths, MigrationReason,
    RecoveryOutcome,
};
use mentisdb::{chain_storage_filename, MentisDb, StorageAdapterKind};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const KEY: &str = "demo";

fn live_path(dir: &Path) -> PathBuf {
    dir.join(chain_storage_filename(KEY, StorageAdapterKind::Binary))
}

fn source_path(dir: &Path, format: FixtureFormat) -> PathBuf {
    let kind = if format.is_jsonl() {
        StorageAdapterKind::Jsonl
    } else {
        StorageAdapterKind::Binary
    };
    dir.join(chain_storage_filename(KEY, kind))
}

/// A directory holding a fixture chain of `count` thoughts in `format`.
fn fixture(format: FixtureFormat, count: usize) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = source_path(dir.path(), format);
    write_fixture(&path, format, &semantic_chain(count)).unwrap();
    (dir, path)
}

fn migrate(dir: &Path) -> Result<MigrationOutcome, MigrationError> {
    run_chain_migration(dir, KEY, &MigrationOptions::default())
}

fn journal(dir: &Path) -> MigrationJournal {
    MigrationJournal::load(&MigrationPaths::new(dir, KEY).journal)
        .unwrap()
        .expect("journal exists")
}

/// `Thought` is not `PartialEq`; compare the full serialized form.
fn assert_same(left: &[mentisdb::Thought], right: &[mentisdb::Thought], label: &str) {
    assert_eq!(left.len(), right.len(), "{label}: length");
    for (a, b) in left.iter().zip(right) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap(),
            "{label}: thought {}",
            a.index
        );
    }
}

fn assert_chain_equals_expected(dir: &Path, format: FixtureFormat, count: usize) {
    let expected = expected_after_migration(format, &semantic_chain(count));
    let chain = MentisDb::open_with_key(dir, KEY).expect("migrated chain opens");
    assert_same(chain.thoughts(), &expected, "expected");
    assert!(chain.verify_integrity());
}

/// Every historical format migrates to exactly the expected thoughts, leaves a
/// finalized journal and a byte-identical backup of the original.
#[test]
fn test_every_format_migrates_to_the_expected_thoughts() {
    for format in FixtureFormat::ALL {
        if format.is_current() {
            continue;
        }
        let (dir, path) = fixture(format, 25);
        let original = std::fs::read(&path).unwrap();
        let outcome = migrate(dir.path()).unwrap_or_else(|e| panic!("{format:?}: {e}"));
        let MigrationOutcome::Migrated(report) = outcome else {
            panic!("{format:?} should migrate");
        };
        assert_eq!(report.thoughts, 25, "{format:?}");
        assert_chain_equals_expected(dir.path(), format, 25);
        assert_eq!(journal(dir.path()).state, JournalState::Finalized);
        let backup = report.backup_dir.unwrap().join(path.file_name().unwrap());
        assert_eq!(
            std::fs::read(backup).unwrap(),
            original,
            "{format:?} backup"
        );
        verify_chain_file(&live_path(dir.path())).unwrap();
        if format.is_jsonl() {
            assert!(
                !path.exists(),
                "the JSONL file is replaced by the binary chain"
            );
        }
    }
}

/// A current chain needs nothing, and a migrated chain needs nothing the second time.
#[test]
fn test_current_and_already_migrated_chains_are_not_touched() {
    let (dir, path) = fixture(FixtureFormat::Current, 10);
    let before = std::fs::read(&path).unwrap();
    assert_eq!(detect_migration(dir.path(), KEY).unwrap(), None);
    assert_eq!(migrate(dir.path()).unwrap(), MigrationOutcome::NotNeeded);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!MigrationPaths::new(dir.path(), KEY).journal.exists());

    let (dir, _) = fixture(FixtureFormat::Schema2, 10);
    assert!(matches!(
        migrate(dir.path()).unwrap(),
        MigrationOutcome::Migrated(_)
    ));
    let after_first = std::fs::read(live_path(dir.path())).unwrap();
    assert_eq!(migrate(dir.path()).unwrap(), MigrationOutcome::NotNeeded);
    assert_eq!(std::fs::read(live_path(dir.path())).unwrap(), after_first);
}

/// Detection names the right reason from the first record alone.
#[test]
fn test_detection_reasons() {
    let cases = [
        (
            FixtureFormat::Schema0,
            Some(MigrationReason::LegacySchema(0)),
        ),
        (
            FixtureFormat::Schema1,
            Some(MigrationReason::LegacySchema(1)),
        ),
        (
            FixtureFormat::Schema2,
            Some(MigrationReason::LegacySchema(2)),
        ),
        (
            FixtureFormat::Schema3Base,
            Some(MigrationReason::LegacyLayout),
        ),
        (
            FixtureFormat::CurrentLegacyHash,
            Some(MigrationReason::LegacyHash),
        ),
        (
            FixtureFormat::JsonlSchema0,
            Some(MigrationReason::JsonlStorage),
        ),
        (FixtureFormat::Current, None),
    ];
    for (format, expected) in cases {
        let (dir, _) = fixture(format, 3);
        assert_eq!(
            detect_migration(dir.path(), KEY).unwrap(),
            expected,
            "{format:?}"
        );
    }
}

/// The record-level pipeline must agree with the decoder `MentisDb::open` has
/// always used, for every format.
#[test]
fn test_engine_output_matches_the_in_place_upgrade_on_open() {
    for format in FixtureFormat::ALL {
        if format.is_jsonl() || format.is_current() {
            continue;
        }
        let (via_engine, _) = fixture(format, 30);
        migrate(via_engine.path()).unwrap();
        let (via_open, _) = fixture(format, 30);
        let opened = MentisDb::open_with_key(via_open.path(), KEY).unwrap();
        let engine_chain = MentisDb::open_with_key(via_engine.path(), KEY).unwrap();
        assert_same(
            engine_chain.thoughts(),
            opened.thoughts(),
            &format!("{format:?}"),
        );
    }
}

/// A kill at every failpoint leaves a recoverable disk: before the commit
/// rename the live chain is the untouched original; after it, the new chain.
/// Recovery (or simply re-running) finishes the job with the expected thoughts.
#[test]
fn test_crash_at_every_step_recovers_to_the_expected_chain() {
    for format in [
        FixtureFormat::Schema1,
        FixtureFormat::Schema3Base,
        FixtureFormat::CurrentLegacyHash,
        FixtureFormat::JsonlSchema0,
    ] {
        for failpoint in Failpoint::all() {
            let (dir, path) = fixture(format, 8);
            let original = std::fs::read(&path).unwrap();

            let crashed =
                run_chain_migration(dir.path(), KEY, &MigrationOptions::crash_at(failpoint));
            assert!(
                matches!(&crashed, Err(e) if e.is_injected()),
                "{format:?} {failpoint}: expected an injected crash, got {crashed:?}"
            );
            let renamed = matches!(
                failpoint,
                Failpoint::AfterCommitRename
                    | Failpoint::AfterCommitJournal
                    | Failpoint::BeforeFinalize
            );
            if !renamed {
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    original,
                    "{format:?} {failpoint}: the original must be untouched before the commit"
                );
            }

            // Recover, then run again; a clean run must end in the expected chain.
            let recovery = recover_chain(dir.path(), KEY, &MigrationOptions::default())
                .unwrap_or_else(|e| panic!("{format:?} {failpoint}: recovery failed: {e}"));
            assert_ne!(
                recovery,
                RecoveryOutcome::Quarantined,
                "{format:?} {failpoint}"
            );
            migrate(dir.path())
                .unwrap_or_else(|e| panic!("{format:?} {failpoint}: rerun failed: {e}"));
            assert_chain_equals_expected(dir.path(), format, 8);
            assert_eq!(
                journal(dir.path()).state,
                JournalState::Finalized,
                "{format:?} {failpoint}"
            );
            let backup = MigrationPaths::new(dir.path(), KEY)
                .backup_dir
                .join(path.file_name().unwrap());
            assert_eq!(
                std::fs::read(backup).unwrap(),
                original,
                "{format:?} {failpoint} backup"
            );
            assert!(
                !MigrationPaths::new(dir.path(), KEY).staged.exists(),
                "{format:?} {failpoint}: no staged leftovers"
            );
        }
    }
}

/// Re-running without calling `recover_chain` first is enough: the run itself
/// resumes whatever the journal says.
#[test]
fn test_run_resumes_an_interrupted_migration_without_explicit_recovery() {
    for failpoint in Failpoint::all() {
        let (dir, _) = fixture(FixtureFormat::Schema1, 6);
        assert!(
            run_chain_migration(dir.path(), KEY, &MigrationOptions::crash_at(failpoint)).is_err()
        );
        migrate(dir.path()).unwrap_or_else(|e| panic!("{failpoint}: {e}"));
        assert_chain_equals_expected(dir.path(), FixtureFormat::Schema1, 6);
    }
}

/// A second crash during recovery is also recoverable.
#[test]
fn test_crash_during_recovery_is_recoverable() {
    let (dir, _) = fixture(FixtureFormat::Schema1, 6);
    assert!(run_chain_migration(
        dir.path(),
        KEY,
        &MigrationOptions::crash_at(Failpoint::AfterCommitJournal)
    )
    .is_err());
    let second = recover_chain(
        dir.path(),
        KEY,
        &MigrationOptions::crash_at(Failpoint::BeforeFinalize),
    );
    assert!(matches!(&second, Err(e) if e.is_injected()), "{second:?}");
    recover_chain(dir.path(), KEY, &MigrationOptions::default()).unwrap();
    assert_chain_equals_expected(dir.path(), FixtureFormat::Schema1, 6);
}

/// A chain that keeps killing the process is quarantined after three attempts
/// instead of crash-looping every start.
#[test]
fn test_repeatedly_interrupted_chain_is_quarantined() {
    let (dir, path) = fixture(FixtureFormat::Schema1, 6);
    let original = std::fs::read(&path).unwrap();
    for _ in 0..3 {
        let crash = MigrationOptions::crash_at(Failpoint::DuringStage { thought: 2 });
        assert!(run_chain_migration(dir.path(), KEY, &crash).is_err());
    }
    let outcome = migrate(dir.path()).unwrap();
    assert!(
        matches!(
            outcome,
            MigrationOutcome::Quarantined {
                state: JournalState::Failed,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "the chain is untouched"
    );
    assert!(journal(dir.path()).error.unwrap().contains("interrupted"));
    // An operator clears it and the migration then succeeds.
    retry_chain(dir.path(), KEY).unwrap();
    migrate(dir.path()).unwrap();
    assert_chain_equals_expected(dir.path(), FixtureFormat::Schema1, 6);
}

/// Flipping a byte in a trusted (schema-3) record is corruption, never silently
/// "fixed": the migration is refused, the chain is byte-identical, the journal
/// says why.
#[test]
fn test_corrupt_hash_is_refused_and_chain_left_untouched() {
    let (dir, path) = fixture(FixtureFormat::Schema3Base, 12);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip a byte inside the content of a record near the middle.
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let error = migrate(dir.path()).unwrap_err();
    assert!(
        matches!(
            error,
            MigrationError::Integrity { .. } | MigrationError::Corrupt { .. }
        ),
        "{error:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "chain untouched");
    assert!(!MigrationPaths::new(dir.path(), KEY).staged.exists());
    let journal = journal(dir.path());
    assert_eq!(journal.state, JournalState::Failed);
    assert!(journal.error.is_some());
    // Quarantined chains are skipped, not retried, on later runs.
    assert!(matches!(
        migrate(dir.path()).unwrap(),
        MigrationOutcome::Quarantined { .. }
    ));
}

/// A tampered legacy-hash chain (stored hash valid under neither algorithm) is refused.
#[test]
fn test_tampered_legacy_hash_chain_is_refused() {
    let (dir, path) = fixture(FixtureFormat::CurrentLegacyHash, 6);
    let mut bytes = std::fs::read(&path).unwrap();
    let n = bytes.len();
    // The last record ends with its hash string; alter its final byte.
    bytes[n - 1] = if bytes[n - 1] == b'0' { b'1' } else { b'0' };
    std::fs::write(&path, &bytes).unwrap();
    let error = migrate(dir.path()).unwrap_err();
    assert!(
        matches!(error, MigrationError::Integrity { index: 5, .. }),
        "{error:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// A torn final record is dropped from the migrated chain (reported, and kept
/// in the backup); every complete record survives.
#[test]
fn test_torn_tail_is_dropped_but_preserved_in_the_backup() {
    let (dir, path) = fixture(FixtureFormat::Schema1, 7);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 5);
    std::fs::write(&path, &bytes).unwrap();
    let MigrationOutcome::Migrated(report) = migrate(dir.path()).unwrap() else {
        panic!("should migrate");
    };
    assert_eq!(report.thoughts, 6);
    assert!(report.torn_tail_bytes > 0);
    let backup = report.backup_dir.unwrap().join(path.file_name().unwrap());
    assert_eq!(std::fs::read(backup).unwrap(), bytes);
}

/// A length prefix that claims an absurd size is corruption, not an allocation.
#[test]
fn test_forged_length_prefix_is_refused() {
    let (dir, path) = fixture(FixtureFormat::Schema1, 3);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    assert!(migrate(dir.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// Truncating a fixture at any byte never panics: it migrates the complete
/// prefix or fails cleanly, and never alters the original.
#[test]
fn test_every_truncation_point_is_safe() {
    let (dir, path) = fixture(FixtureFormat::Schema2, 4);
    let full = std::fs::read(&path).unwrap();
    for cut in (0..full.len()).step_by(7) {
        let scratch = TempDir::new().unwrap();
        let target = source_path(scratch.path(), FixtureFormat::Schema2);
        std::fs::write(&target, &full[..cut]).unwrap();
        let _ = run_chain_migration(scratch.path(), KEY, &MigrationOptions::default());
        if let Ok(Some(_)) =
            MigrationJournal::load(&MigrationPaths::new(scratch.path(), KEY).journal)
        {
            // Whatever happened, the backup is a faithful copy of the input.
            let backup = MigrationPaths::new(scratch.path(), KEY)
                .backup_dir
                .join(target.file_name().unwrap());
            if backup.exists() {
                assert_eq!(std::fs::read(backup).unwrap(), &full[..cut]);
            }
        }
    }
    drop(dir);
}

/// A chain written by a newer release is refused with a clear message and never modified.
#[test]
fn test_future_schema_is_refused_clearly() {
    let (dir, path) = fixture(FixtureFormat::Current, 3);
    let mut bytes = std::fs::read(&path).unwrap();
    // The first record's payload starts at byte 8 with the schema varint (3).
    assert_eq!(bytes[8], 3);
    bytes[8] = 9;
    std::fs::write(&path, &bytes).unwrap();
    let error = detect_migration(dir.path(), KEY).unwrap_err();
    assert!(
        matches!(error, MigrationError::FutureSchema { version: 9 }),
        "{error:?}"
    );
    assert!(error.to_string().contains("upgrade mentisdb"));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// Rollback restores the original bytes and registry; it refuses once new
/// thoughts were appended unless forced.
#[test]
fn test_rollback_restores_the_original_and_refuses_after_appends() {
    let (dir, path) = fixture(FixtureFormat::Schema1, 5);
    let original = std::fs::read(&path).unwrap();
    migrate(dir.path()).unwrap();
    rollback_chain(dir.path(), KEY, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(journal(dir.path()).state, JournalState::RolledBack);
    assert!(matches!(
        migrate(dir.path()).unwrap(),
        MigrationOutcome::Quarantined {
            state: JournalState::RolledBack,
            ..
        }
    ));

    retry_chain(dir.path(), KEY).unwrap();
    migrate(dir.path()).unwrap();
    {
        let mut chain = MentisDb::open_with_key(dir.path(), KEY).unwrap();
        chain
            .append(
                "agent-0",
                mentisdb::ThoughtType::Finding,
                "added after migrating",
            )
            .unwrap();
    }
    let error = rollback_chain(dir.path(), KEY, false).unwrap_err();
    assert!(matches!(error, MigrationError::Refused(_)), "{error:?}");
    assert!(error.to_string().contains("--force"));
    rollback_chain(dir.path(), KEY, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

/// Skip parks a chain untouched; retry brings it back.
#[test]
fn test_skip_and_retry() {
    let (dir, path) = fixture(FixtureFormat::Schema1, 4);
    let original = std::fs::read(&path).unwrap();
    skip_chain(dir.path(), KEY, "maintenance window").unwrap();
    assert!(matches!(
        migrate(dir.path()).unwrap(),
        MigrationOutcome::Quarantined {
            state: JournalState::Skipped,
            ..
        }
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    retry_chain(dir.path(), KEY).unwrap();
    assert!(matches!(
        migrate(dir.path()).unwrap(),
        MigrationOutcome::Migrated(_)
    ));
}

/// A dry run writes nothing, and reports what a real run would do.
#[test]
fn test_dry_run_writes_nothing_and_reports() {
    let (dir, path) = fixture(FixtureFormat::Schema2, 9);
    let before = std::fs::read(&path).unwrap();
    let report = dry_run_chain(dir.path(), KEY, &MigrationOptions::default())
        .unwrap()
        .expect("needs migration");
    assert_eq!(report.thoughts, 9);
    assert_eq!(report.from_schema, 2);
    assert!(report.hash_changed);
    assert!(report.steps.contains(&"relations-v2-to-v3".to_owned()));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(
        !dir.path().join("migrations").exists(),
        "no migrations directory created"
    );

    let (corrupt, cpath) = fixture(FixtureFormat::Schema3Base, 6);
    let mut bytes = std::fs::read(&cpath).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&cpath, bytes).unwrap();
    assert!(dry_run_chain(corrupt.path(), KEY, &MigrationOptions::default()).is_err());
}

/// Two migrators cannot work on one chain at once.
#[test]
fn test_second_migrator_gets_busy() {
    let (dir, _) = fixture(FixtureFormat::Schema1, 3);
    let paths = MigrationPaths::new(dir.path(), KEY);
    std::fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.lock)
        .unwrap();
    held.lock().unwrap();
    let error = migrate(dir.path()).unwrap_err();
    assert!(matches!(error, MigrationError::Busy { .. }), "{error:?}");
    drop(held);
    migrate(dir.path()).unwrap();
}

/// Derived files are invalidated when hashes change, and kept when they do not.
#[test]
fn test_derived_files_are_invalidated_only_when_hashes_change() {
    for (format, hashes_change) in [
        (FixtureFormat::Schema1, true),
        (FixtureFormat::Schema3Base, false),
    ] {
        let (dir, path) = fixture(format, 4);
        let stem = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".tcbin", "");
        let sidecar = dir.path().join(format!("{stem}.vectors.m.v1.8d.json"));
        let overlay = dir.path().join(format!("{stem}.auto_edges.bin"));
        let config = dir.path().join(format!("{stem}.vectors.managed.json"));
        for file in [&sidecar, &overlay, &config] {
            std::fs::write(file, b"x").unwrap();
        }
        migrate(dir.path()).unwrap();
        assert_eq!(sidecar.exists(), !hashes_change, "{format:?} sidecar");
        assert_eq!(overlay.exists(), !hashes_change, "{format:?} overlay");
        assert!(
            config.exists(),
            "{format:?}: the sidecar configuration is always kept"
        );
    }
}

/// The registry entry is updated to the current version and thought count.
#[test]
fn test_registry_entry_is_updated() {
    let (dir, _) = fixture(FixtureFormat::Schema1, 11);
    migrate(dir.path()).unwrap();
    let registry = mentisdb::load_registered_chains(dir.path()).unwrap();
    let entry = registry.chains.get(KEY).expect("registered");
    assert_eq!(entry.version, mentisdb::MENTISDB_CURRENT_VERSION);
    assert_eq!(entry.thought_count, 11);
    assert_eq!(entry.storage_adapter, StorageAdapterKind::Binary);
}

/// Progress is reported while staging.
#[test]
fn test_progress_reaches_one_hundred_percent() {
    let (dir, _) = fixture(FixtureFormat::Schema1, 5000);
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&seen);
    let options = MigrationOptions {
        failpoint: None,
        progress: Some(std::sync::Arc::new(
            move |p: &mentisdb::migration::MigrationProgress| {
                sink.lock().unwrap().push((p.step, p.percent()));
            },
        )),
    };
    run_chain_migration(dir.path(), KEY, &options).unwrap();
    let seen = seen.lock().unwrap();
    assert!(seen
        .iter()
        .any(|(step, p)| *step == mentisdb::migration::MigrationStep::Staging && *p == 100));
    assert!(
        seen.windows(2)
            .filter(|w| w[0].0 == w[1].0)
            .all(|w| w[0].1 <= w[1].1),
        "monotonic"
    );
}
