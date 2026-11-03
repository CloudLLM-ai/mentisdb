//! Day counts typed by an operator must saturate, not overflow.
//!
//! `days * 86_400` panics in a debug build and wraps to a small number in a
//! release build. The wrapped value meant "keep migration backups for a few
//! seconds", so an absurdly long retention purged every backup. These tests
//! assert the resulting duration, so they catch the wrap in release builds too
//! (a test that only expects "no panic" would pass there for the wrong reason).

#![cfg(feature = "server")]

use mentisdb::migration::fixtures::{semantic_chain, write_fixture, FixtureFormat};
use mentisdb::migration::{
    run_chain_migration, MigrationConfig, MigrationOptions, MigrationOutcome, MigrationPaths,
};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

const RETENTION_ENV: &str = "MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS";

/// Serializes the tests that set a process-wide environment variable.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The first day count whose seconds do not fit in a `u64`.
const OVERFLOWING_DAYS: u64 = u64::MAX / 86_400 + 1;

/// A day count whose seconds wrap to exactly zero in a release build
/// (`86_400 = 2^7 * 675`, so `2^57 * 86_400` is `675 * 2^64`).
const WRAPS_TO_ZERO_DAYS: u64 = 1 << 57;

fn retention_with(value: &str) -> Option<Duration> {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var(RETENTION_ENV, value);
    let retention = MigrationConfig::new("unused").backup_retention;
    std::env::remove_var(RETENTION_ENV);
    retention
}

/// Ordinary retention values convert exactly, and `0` still means forever.
#[test]
fn test_retention_days_convert_to_seconds() {
    assert_eq!(retention_with("14"), Some(Duration::from_secs(14 * 86_400)));
    assert_eq!(retention_with("0"), None);
}

/// A retention too long to represent saturates to the longest duration instead
/// of panicking (debug) or wrapping to a few seconds (release).
#[test]
fn test_overflowing_retention_saturates() {
    for days in [OVERFLOWING_DAYS, u64::MAX] {
        assert_eq!(
            retention_with(&days.to_string()),
            Some(Duration::from_secs(u64::MAX)),
            "{days} days"
        );
    }
}

/// `purge-backups --older-than-days <huge>` means "older than anything can be"
/// and keeps the backup. With the wrapped value (zero seconds for
/// `WRAPS_TO_ZERO_DAYS`) a release build deleted a backup written moments ago.
#[test]
fn test_purge_with_overflowing_days_keeps_recent_backups() {
    let dir = TempDir::new().unwrap();
    write_fixture(
        &dir.path()
            .join(chain_storage_filename("alpha", StorageAdapterKind::Binary)),
        FixtureFormat::Schema1,
        &semantic_chain(6),
    )
    .unwrap();
    let outcome = run_chain_migration(dir.path(), "alpha", &MigrationOptions::default()).unwrap();
    assert!(matches!(outcome, MigrationOutcome::Migrated(_)));
    let backup = MigrationPaths::new(dir.path(), "alpha").backup_dir;
    assert!(backup.exists(), "the migration preserved the original");

    let dir_arg = dir.path().to_str().unwrap();
    let days = WRAPS_TO_ZERO_DAYS.to_string();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = mentisdb::cli::run_with_io(
        [
            "mentisdb",
            "migrate",
            "purge-backups",
            "--dir",
            dir_arg,
            "--older-than-days",
            &days,
        ],
        &mut std::io::empty(),
        &mut out,
        &mut err,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "{out} {}",
        String::from_utf8_lossy(&err)
    );
    assert!(out.contains("removed 0 backup"), "{out}");
    assert!(backup.exists(), "a recent backup must survive");
}
