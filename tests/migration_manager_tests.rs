//! Tests for the background migration manager: startup returns immediately,
//! requests to a migrating chain get a precise answer, failures are quarantined
//! without touching other chains, and interrupted work resumes on start.

use mentisdb::migration::fixtures::{
    expected_after_migration, semantic_chain, write_fixture, write_large_fixture, FixtureFormat,
};
use mentisdb::migration::{
    check_format_stamp, run_chain_migration, Failpoint, MigrationBlock, MigrationConfig,
    MigrationError, MigrationManager, MigrationOptions, MigrationPhase,
};
use mentisdb::{chain_storage_filename, MentisDb, StorageAdapterKind};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn put(dir: &Path, key: &str, format: FixtureFormat, count: usize) {
    let kind = if format.is_jsonl() {
        StorageAdapterKind::Jsonl
    } else {
        StorageAdapterKind::Binary
    };
    write_fixture(
        &dir.join(chain_storage_filename(key, kind)),
        format,
        &semantic_chain(count),
    )
    .unwrap();
}

fn config(dir: &Path) -> MigrationConfig {
    let mut config = MigrationConfig::new(dir);
    config.max_concurrency = 2;
    config.backup_retention = None;
    config
}

/// A hook that sleeps at the first record of a chain, to hold migrations open.
fn slow(ms: u64) -> MigrationOptions {
    MigrationOptions {
        failpoint: Some(Arc::new(move |fp| {
            if matches!(fp, Failpoint::DuringStage { thought: 0 }) {
                std::thread::sleep(Duration::from_millis(ms));
            }
            false
        })),
        progress: None,
    }
}

/// `start` returns before any chain is rewritten; the chain is gated with a
/// percent and a retry hint until its migration finishes, then served.
#[test]
fn test_start_is_non_blocking_and_gates_migrating_chains() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "old", FixtureFormat::Schema1, 50);
    let mut cfg = config(dir.path());
    cfg.options = slow(400);
    let started = Instant::now();
    let manager = MigrationManager::start(cfg).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "start must not wait for the migration"
    );

    match manager.gate("old") {
        Some(MigrationBlock::Migrating {
            retry_after_secs, ..
        }) => assert!(retry_after_secs >= 1),
        other => panic!("expected a migrating block, got {other:?}"),
    }
    assert!(
        manager.gate("unrelated-chain").is_none(),
        "other chains are never blocked"
    );
    assert!(manager.wait_idle(Duration::from_secs(30)));
    assert!(manager.gate("old").is_none());
    let status = manager.status("old").unwrap();
    assert_eq!(status.phase, MigrationPhase::Completed);
    assert_eq!(status.percent, 100);
    assert_eq!(status.report.unwrap().thoughts, 50);
    let chain = MentisDb::open_with_key(dir.path(), "old").unwrap();
    assert_eq!(chain.thoughts().len(), 50);
}

/// The block's message is what clients see: it must say what to do.
#[test]
fn test_block_messages_are_actionable() {
    let migrating = MigrationBlock::Migrating {
        chain_key: "c".into(),
        percent: 42,
        retry_after_secs: 5,
    };
    assert!(
        migrating.to_string().contains("42%") && migrating.to_string().contains("retry after 5")
    );
    assert_eq!(migrating.retry_after_secs(), Some(5));
    let quarantined = MigrationBlock::Quarantined {
        chain_key: "c".into(),
        error: "boom".into(),
    };
    assert!(quarantined.to_string().contains("migrate retry c"));
    assert_eq!(quarantined.retry_after_secs(), None);
}

/// One corrupt chain is quarantined with its error; the others migrate and the
/// daemon (here: the manager) keeps going. The quarantine survives a restart.
#[test]
fn test_failing_chain_is_quarantined_and_does_not_affect_others() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "good-a", FixtureFormat::Schema1, 10);
    put(dir.path(), "good-b", FixtureFormat::Schema2, 10);
    put(dir.path(), "bad", FixtureFormat::Schema3Base, 10);
    let bad_path = dir
        .path()
        .join(chain_storage_filename("bad", StorageAdapterKind::Binary));
    let mut bytes = std::fs::read(&bad_path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&bad_path, &bytes).unwrap();

    let manager = MigrationManager::start(config(dir.path())).unwrap();
    assert!(manager.wait_idle(Duration::from_secs(30)));
    assert_eq!(
        manager.status("good-a").unwrap().phase,
        MigrationPhase::Completed
    );
    assert_eq!(
        manager.status("good-b").unwrap().phase,
        MigrationPhase::Completed
    );
    let bad = manager.status("bad").unwrap();
    assert_eq!(bad.phase, MigrationPhase::Failed);
    assert!(bad.error.is_some());
    assert!(matches!(
        manager.gate("bad"),
        Some(MigrationBlock::Quarantined { .. })
    ));
    assert_eq!(
        std::fs::read(&bad_path).unwrap(),
        bytes,
        "quarantined data is untouched"
    );
    manager.shutdown();

    // A restarted daemon still reports the quarantine and does not retry it.
    let restarted = MigrationManager::start(config(dir.path())).unwrap();
    assert!(restarted.wait_idle(Duration::from_secs(10)));
    assert_eq!(
        restarted.status("bad").unwrap().phase,
        MigrationPhase::Failed
    );
    assert!(matches!(
        restarted.gate("bad"),
        Some(MigrationBlock::Quarantined { .. })
    ));
    assert!(restarted.gate("good-a").is_none());

    // Fix the file and retry through the manager.
    bytes[middle] ^= 0x01;
    std::fs::write(&bad_path, &bytes).unwrap();
    restarted.retry("bad").unwrap();
    assert!(restarted.wait_idle(Duration::from_secs(30)));
    assert_eq!(
        restarted.status("bad").unwrap().phase,
        MigrationPhase::Completed
    );
    assert!(restarted.gate("bad").is_none());
}

/// Skip leaves a chain alone and unavailable, across restarts; retry migrates it.
#[test]
fn test_skip_then_retry_through_the_manager() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "later", FixtureFormat::Schema1, 5);
    let path = dir
        .path()
        .join(chain_storage_filename("later", StorageAdapterKind::Binary));
    let original = std::fs::read(&path).unwrap();
    mentisdb::migration::skip_chain(dir.path(), "later", "maintenance window").unwrap();

    let manager = MigrationManager::start(config(dir.path())).unwrap();
    assert!(manager.wait_idle(Duration::from_secs(5)));
    assert_eq!(
        manager.status("later").unwrap().phase,
        MigrationPhase::Skipped
    );
    assert!(matches!(
        manager.gate("later"),
        Some(MigrationBlock::Skipped { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);

    manager.retry("later").unwrap();
    assert!(manager.wait_idle(Duration::from_secs(30)));
    assert!(manager.gate("later").is_none());
    assert_eq!(
        manager.status("later").unwrap().phase,
        MigrationPhase::Completed
    );
}

/// Concurrency is bounded by `max_concurrency`.
#[test]
fn test_concurrency_is_bounded() {
    let dir = TempDir::new().unwrap();
    for n in 0..6 {
        put(
            dir.path(),
            &format!("chain-{n}"),
            FixtureFormat::Schema1,
            20,
        );
    }
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r, p) = (Arc::clone(&running), Arc::clone(&peak));
    let mut cfg = config(dir.path());
    cfg.max_concurrency = 2;
    cfg.options = MigrationOptions {
        failpoint: Some(Arc::new(move |fp| {
            match fp {
                Failpoint::AfterPlan => {
                    let now = r.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                }
                Failpoint::BeforeFinalize => {
                    r.fetch_sub(1, Ordering::SeqCst);
                }
                _ => {}
            }
            false
        })),
        progress: None,
    };
    let manager = MigrationManager::start(cfg).unwrap();
    assert!(manager.wait_idle(Duration::from_secs(60)));
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "peak {}",
        peak.load(Ordering::SeqCst)
    );
    assert!(peak.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        manager
            .statuses()
            .iter()
            .filter(|s| s.phase == MigrationPhase::Completed)
            .count(),
        6
    );
}

/// A migration the previous process died in is resumed when the manager starts.
#[test]
fn test_start_resumes_interrupted_migrations() {
    for failpoint in [
        Failpoint::AfterBackup,
        Failpoint::DuringStage { thought: 3 },
        Failpoint::BeforeCommitRename,
        Failpoint::AfterCommitRename,
        Failpoint::AfterCommitJournal,
    ] {
        let dir = TempDir::new().unwrap();
        put(dir.path(), "resume", FixtureFormat::Schema1, 12);
        assert!(
            run_chain_migration(dir.path(), "resume", &MigrationOptions::crash_at(failpoint))
                .is_err()
        );
        let manager = MigrationManager::start(config(dir.path())).unwrap();
        assert!(manager.wait_idle(Duration::from_secs(30)), "{failpoint}");
        assert!(manager.gate("resume").is_none(), "{failpoint}");
        let chain = MentisDb::open_with_key(dir.path(), "resume").unwrap();
        let expected = expected_after_migration(FixtureFormat::Schema1, &semantic_chain(12));
        assert_eq!(chain.thoughts().len(), expected.len(), "{failpoint}");
        assert_eq!(
            chain.head_hash(),
            expected.last().map(|t| t.hash.as_str()),
            "{failpoint}"
        );
    }
}

/// Readers hammering the gate while a migration runs only ever see "migrating"
/// or a complete, correct chain: never a partial or half-written one.
#[test]
fn test_concurrent_requests_during_migration_never_see_a_partial_chain() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "busy", FixtureFormat::Schema1, 300);
    let mut cfg = config(dir.path());
    cfg.options = slow(300);
    let manager = MigrationManager::start(cfg).unwrap();
    let blocked = Arc::new(AtomicUsize::new(0));
    let served = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (manager, blocked, served, path) = (
                manager.clone(),
                Arc::clone(&blocked),
                Arc::clone(&served),
                dir.path().to_path_buf(),
            );
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                while Instant::now() < deadline {
                    match manager.gate("busy") {
                        Some(MigrationBlock::Migrating { .. }) => {
                            blocked.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        None => {
                            let chain = MentisDb::open_with_key(&path, "busy").unwrap();
                            assert_eq!(chain.thoughts().len(), 300);
                            assert!(chain.verify_integrity());
                            served.fetch_add(1, Ordering::SeqCst);
                            return;
                        }
                        Some(other) => panic!("unexpected block {other:?}"),
                    }
                }
                panic!("migration never finished");
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
    assert!(
        blocked.load(Ordering::SeqCst) > 0,
        "readers were told to retry while it ran"
    );
    assert_eq!(served.load(Ordering::SeqCst), 4);
}

/// A data directory stamped by a newer release is refused, untouched.
#[test]
fn test_newer_format_stamp_is_refused() {
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join(mentisdb::migration::FORMAT_STAMP_FILENAME),
        r#"{"format": 99, "written_by": "9.9.9", "written_at": "2030-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let error = check_format_stamp(dir.path()).unwrap_err();
    assert!(matches!(
        error,
        MigrationError::FutureSchema { version: 99 }
    ));
    assert!(MigrationManager::start(config(dir.path())).is_err());
    assert!(
        std::fs::read_to_string(dir.path().join(mentisdb::migration::FORMAT_STAMP_FILENAME))
            .unwrap()
            .contains("99"),
        "the newer stamp is never overwritten"
    );
}

/// Starting stamps the directory so later releases can recognise it.
#[test]
fn test_start_writes_the_format_stamp() {
    let dir = TempDir::new().unwrap();
    let manager = MigrationManager::start(config(dir.path())).unwrap();
    assert!(manager.wait_idle(Duration::from_secs(5)));
    assert!(dir
        .path()
        .join(mentisdb::migration::FORMAT_STAMP_FILENAME)
        .exists());
}

/// Time a large chain end to end and keep the flat-memory promise honest: the
/// result is checked, the numbers are printed (run with `--nocapture`).
///
/// Size via `MENTISDB_MIGRATION_TEST_THOUGHTS` (default 150 000).
#[test]
fn test_large_chain_migrates_and_is_timed() {
    let count: usize = std::env::var("MENTISDB_MIGRATION_TEST_THOUGHTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150_000);
    let dir = TempDir::new().unwrap();
    let path = dir
        .path()
        .join(chain_storage_filename("large", StorageAdapterKind::Binary));
    write_large_fixture(&path, FixtureFormat::Schema1, count).unwrap();
    let bytes = std::fs::metadata(&path).unwrap().len();

    let started = Instant::now();
    let manager = MigrationManager::start(config(dir.path())).unwrap();
    let start_latency = started.elapsed();
    assert!(manager.wait_idle(Duration::from_secs(900)));
    let total = started.elapsed();

    let status = manager.status("large").unwrap();
    assert_eq!(
        status.phase,
        MigrationPhase::Completed,
        "{:?}",
        status.error
    );
    let report = status.report.unwrap();
    assert_eq!(report.thoughts as usize, count);
    mentisdb::migration::verify_chain_file(
        &dir.path()
            .join(chain_storage_filename("large", StorageAdapterKind::Binary)),
    )
    .unwrap();
    println!(
        "MIGRATION large chain: {count} thoughts, {:.1} MiB -> {:.1} MiB; start() returned in {:?}; \
         total {:?} ({:.0} thoughts/s, {:.1} MiB/s)",
        bytes as f64 / 1_048_576.0,
        report.staged_bytes as f64 / 1_048_576.0,
        start_latency,
        total,
        count as f64 / total.as_secs_f64(),
        bytes as f64 / 1_048_576.0 / total.as_secs_f64()
    );
    assert!(
        start_latency < Duration::from_secs(2),
        "startup must stay non-blocking"
    );
}

/// Options whose hook parks the worker at the first record of a chain: it tells
/// `entered` the migration is under way, then waits for `release`.
fn parked(entered: Sender<()>, release: Receiver<()>) -> MigrationOptions {
    let entered = Mutex::new(entered);
    let release = Mutex::new(release);
    MigrationOptions {
        failpoint: Some(Arc::new(move |fp| {
            if matches!(fp, Failpoint::DuringStage { thought: 0 }) {
                let _ = entered.lock().unwrap().send(());
                let _ = release.lock().unwrap().recv();
            }
            false
        })),
        progress: None,
    }
}

/// Stopping the daemon while a chain migrates (`SIGTERM`, a restart) is a clean
/// stop, not a crash: the manager halts the running migration, the chain is
/// untouched and waits as `Pending`, and no attempt is charged, so any number of
/// restarts (well over the three that quarantine a crashing chain) never
/// quarantine it. The start after them migrates it.
#[test]
fn test_shutdown_during_migration_does_not_count_toward_quarantine() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "slowpoke", FixtureFormat::Schema1, 50);
    let live = dir.path().join(chain_storage_filename(
        "slowpoke",
        StorageAdapterKind::Binary,
    ));
    let original = std::fs::read(&live).unwrap();
    for restart in 0..5 {
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let mut cfg = config(dir.path());
        cfg.options = parked(entered_tx, release_rx);
        let manager = MigrationManager::start(cfg).unwrap();
        entered_rx.recv().unwrap();
        // The migration is mid-flight and parked; stop the daemon.
        manager.shutdown();
        release_tx.send(()).unwrap();
        assert!(
            manager.wait_stopped(Duration::from_secs(60)),
            "restart {restart}"
        );
        let status = manager.status("slowpoke").unwrap();
        assert_eq!(
            status.phase,
            MigrationPhase::Pending,
            "restart {restart}: {status:?}"
        );
        assert_eq!(status.error, None);
        assert_eq!(std::fs::read(&live).unwrap(), original, "chain untouched");
    }
    let manager = MigrationManager::start(config(dir.path())).unwrap();
    assert!(manager.wait_idle(Duration::from_secs(60)));
    let status = manager.status("slowpoke").unwrap();
    assert_eq!(status.phase, MigrationPhase::Completed, "{status:?}");
    assert_eq!(status.attempts, 1, "only the run that finished is counted");
    assert!(manager.gate("slowpoke").is_none());
    assert_eq!(
        MentisDb::open_with_key(dir.path(), "slowpoke")
            .unwrap()
            .thoughts()
            .len(),
        50
    );
}

/// `wait_stopped` reports true once every worker has exited.
#[test]
fn test_wait_stopped_returns_after_shutdown() {
    let dir = TempDir::new().unwrap();
    let manager = MigrationManager::start(config(dir.path())).unwrap();
    manager.shutdown();
    assert!(manager.wait_stopped(Duration::from_secs(30)));
}
