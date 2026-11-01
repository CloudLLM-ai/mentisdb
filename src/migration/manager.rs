//! The background migration manager: never blocks startup, bounds concurrency,
//! quarantines failures and reports every chain's state.
//!
//! [`MigrationManager::start`] does only O(chains) work before returning (it
//! reads each chain's first record and replays nothing); the actual rewrites
//! run on a small pool of worker threads. The server consults
//! [`MigrationManager::gate`] before opening a chain, so a request for a chain
//! that is still migrating gets a precise answer instead of a hang or a
//! half-migrated read.

use super::engine::{self, list_journals, JournalState, MigrationJournal, MigrationReport};
use super::{
    mig_log, MigrationError, MigrationOptions, MigrationOutcome, MigrationProgress, MigrationStep,
};
use crate::{chain_storage_filename, StorageAdapterKind, MENTISDB_CURRENT_VERSION};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Name of the format stamp file in the storage directory.
pub const FORMAT_STAMP_FILENAME: &str = "mentisdb-format.json";

/// Default retention of preserved originals after a successful migration.
const DEFAULT_BACKUP_RETENTION_DAYS: u64 = 14;

#[derive(Debug, Serialize, Deserialize)]
struct FormatStamp {
    format: u32,
    written_by: String,
    written_at: DateTime<Utc>,
}

/// Record the format this release writes in the storage directory.
///
/// Older releases ignore the file; this and newer releases read it through
/// [`check_format_stamp`] so a directory touched by a *newer* release is never
/// opened (and so never corrupted) by an older one that has this check. The
/// stamp only ever moves forward: an existing newer stamp is left alone.
///
/// # Errors
///
/// Filesystem errors.
pub fn write_format_stamp(chain_dir: &Path) -> io::Result<()> {
    if read_stamp(chain_dir).is_some_and(|stamp| stamp.format > MENTISDB_CURRENT_VERSION) {
        return Ok(());
    }
    fs::create_dir_all(chain_dir)?;
    let stamp = FormatStamp {
        format: MENTISDB_CURRENT_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_owned(),
        written_at: Utc::now(),
    };
    let path = chain_dir.join(FORMAT_STAMP_FILENAME);
    let temp = chain_dir.join(format!(
        "{FORMAT_STAMP_FILENAME}.tmp-{}",
        std::process::id()
    ));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(&stamp).map_err(io::Error::other)?,
    )?;
    fs::rename(temp, path)
}

fn read_stamp(chain_dir: &Path) -> Option<FormatStamp> {
    serde_json::from_slice(&fs::read(chain_dir.join(FORMAT_STAMP_FILENAME)).ok()?).ok()
}

/// Refuse a storage directory written by a newer mentisdb.
///
/// # Errors
///
/// [`MigrationError::FutureSchema`] when the stamp names a newer format than
/// this build supports. A missing or unreadable stamp is fine.
///
/// # Examples
///
/// ```
/// use mentisdb::migration::{check_format_stamp, write_format_stamp};
///
/// let dir = tempfile::tempdir().unwrap();
/// assert!(check_format_stamp(dir.path()).is_ok());
/// write_format_stamp(dir.path()).unwrap();
/// assert!(check_format_stamp(dir.path()).is_ok());
/// ```
pub fn check_format_stamp(chain_dir: &Path) -> Result<(), MigrationError> {
    match read_stamp(chain_dir) {
        Some(stamp) if stamp.format > MENTISDB_CURRENT_VERSION => {
            Err(MigrationError::FutureSchema {
                version: stamp.format,
            })
        }
        _ => Ok(()),
    }
}

/// Chain keys worth examining in `chain_dir`: registered chains, chain files
/// on disk whose name maps back to a key, and chains with a migration journal.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// assert!(mentisdb::migration::discover_chain_keys(dir.path()).is_empty());
/// ```
pub fn discover_chain_keys(chain_dir: &Path) -> BTreeSet<String> {
    let dir = chain_dir;
    let mut keys = BTreeSet::new();
    if let Ok(registry) = crate::load_registered_chains(dir) {
        keys.extend(registry.chains.keys().cloned());
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(key) = crate::chain_key_from_storage_filename(name) else {
                continue;
            };
            // Only keys that map back to this very file are addressable;
            // others (sanitised keys) are known through the registry.
            let matches = [StorageAdapterKind::Binary, StorageAdapterKind::Jsonl]
                .into_iter()
                .any(|kind| chain_storage_filename(&key, kind) == name);
            if !key.is_empty() && matches {
                keys.insert(key);
            }
        }
    }
    keys.extend(list_journals(dir).into_iter().map(|j| j.chain_key));
    keys
}

/// Managers by storage directory, so surfaces that do not share the service
/// (the dashboard opens chains itself) can find the manager of their directory.
static MANAGERS: Mutex<BTreeMap<PathBuf, MigrationManager>> = Mutex::new(BTreeMap::new());

fn registry_key(chain_dir: &Path) -> PathBuf {
    fs::canonicalize(chain_dir).unwrap_or_else(|_| chain_dir.to_path_buf())
}

/// The migration manager started for `chain_dir` in this process, if any.
///
/// # Examples
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// assert!(mentisdb::migration::manager_for_dir(dir.path()).is_none());
/// ```
pub fn manager_for_dir(chain_dir: &Path) -> Option<MigrationManager> {
    MANAGERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&registry_key(chain_dir))
        .cloned()
}

/// Where a chain is in the migration pipeline, as reported to operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    /// Queued; not started yet. Requests for the chain are told to retry.
    Pending,
    /// A worker is migrating it now.
    Running,
    /// Migrated successfully this run (or by an earlier run).
    Completed,
    /// Quarantined with an error; the chain is untouched and unavailable.
    Failed,
    /// Left alone on the operator's request.
    Skipped,
    /// Restored from backup; unavailable until `retry`.
    RolledBack,
}

impl MigrationPhase {
    /// True while the chain cannot be served.
    pub fn blocks_requests(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

/// One chain's migration state, the unit of every status surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChainMigrationStatus {
    /// The chain.
    pub chain_key: String,
    /// Where it is.
    pub phase: MigrationPhase,
    /// Current step while running.
    pub step: Option<MigrationStep>,
    /// Whole-percent progress through the staging pass.
    pub percent: u8,
    /// Records written so far.
    pub thoughts_done: u64,
    /// Why the chain needed migrating.
    pub reason: Option<String>,
    /// Attempts so far (resumes included).
    pub attempts: u32,
    /// Failure text for `Failed`, `Skipped` and `RolledBack`.
    pub error: Option<String>,
    /// When work on it began.
    pub started_at: Option<DateTime<Utc>>,
    /// When it reached a terminal phase.
    pub finished_at: Option<DateTime<Utc>>,
    /// Where the original is preserved.
    pub backup_dir: Option<String>,
    /// Summary once completed.
    pub report: Option<MigrationReport>,
}

impl ChainMigrationStatus {
    fn new(chain_key: &str, phase: MigrationPhase) -> Self {
        Self {
            chain_key: chain_key.to_owned(),
            phase,
            step: None,
            percent: 0,
            thoughts_done: 0,
            reason: None,
            attempts: 0,
            error: None,
            started_at: None,
            finished_at: None,
            backup_dir: None,
            report: None,
        }
    }
}

/// Why a request for a chain cannot be served right now.
///
/// Returned by [`MigrationManager::gate`]; the server maps it to HTTP 503
/// (`Retry-After` for [`Self::Migrating`]) and an MCP error with the same text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MigrationBlock {
    /// The chain is queued or being migrated.
    Migrating {
        /// Chain key.
        chain_key: String,
        /// Whole-percent progress.
        percent: u8,
        /// Suggested client back-off in seconds.
        retry_after_secs: u64,
    },
    /// The chain failed to migrate and is quarantined.
    Quarantined {
        /// Chain key.
        chain_key: String,
        /// The recorded error.
        error: String,
    },
    /// The operator skipped the chain.
    Skipped {
        /// Chain key.
        chain_key: String,
    },
    /// The chain was rolled back and awaits `migrate retry`.
    RolledBack {
        /// Chain key.
        chain_key: String,
    },
}

impl MigrationBlock {
    /// Suggested `Retry-After` for clients, when waiting can help.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Migrating {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

impl std::fmt::Display for MigrationBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Migrating {
                chain_key,
                percent,
                retry_after_secs,
            } => write!(
                f,
                "chain '{chain_key}' is being migrated to the new storage format ({percent}% done); \
                 retry after {retry_after_secs} seconds"
            ),
            Self::Quarantined { chain_key, error } => write!(
                f,
                "chain '{chain_key}' failed to migrate and is quarantined (its data is untouched): \
                 {error}. Run `mentisdb migrate status` and `mentisdb migrate retry {chain_key}`"
            ),
            Self::Skipped { chain_key } => write!(
                f,
                "chain '{chain_key}' was skipped by an operator; run `mentisdb migrate retry \
                 {chain_key}` to migrate it"
            ),
            Self::RolledBack { chain_key } => write!(
                f,
                "chain '{chain_key}' was rolled back to its pre-migration backup; run \
                 `mentisdb migrate retry {chain_key}` to migrate it again"
            ),
        }
    }
}

impl std::error::Error for MigrationBlock {}

/// Configuration for [`MigrationManager::start`].
///
/// # Examples
///
/// ```
/// use mentisdb::migration::MigrationConfig;
///
/// let config = MigrationConfig::new("/tmp/mentisdb");
/// assert!(config.max_concurrency >= 1);
/// ```
#[derive(Clone)]
pub struct MigrationConfig {
    /// Storage directory.
    pub chain_dir: PathBuf,
    /// Worker threads (chains migrated at once). `MENTISDB_MIGRATION_CONCURRENCY`, default 2.
    pub max_concurrency: usize,
    /// How long to keep preserved originals after success.
    /// `MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS`, default 14; `0` keeps them forever.
    pub backup_retention: Option<Duration>,
    /// Engine options (failpoints in tests, extra progress observers).
    pub options: MigrationOptions,
}

impl MigrationConfig {
    /// Defaults read from the environment.
    pub fn new(chain_dir: impl Into<PathBuf>) -> Self {
        let env_u64 = |name: &str| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let days = env_u64("MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS")
            .unwrap_or(DEFAULT_BACKUP_RETENTION_DAYS);
        Self {
            chain_dir: chain_dir.into(),
            max_concurrency: env_u64("MENTISDB_MIGRATION_CONCURRENCY")
                .map(|n| n.max(1) as usize)
                .unwrap_or(2),
            backup_retention: (days > 0).then(|| Duration::from_secs(days * 86_400)),
            options: MigrationOptions::default(),
        }
    }
}

struct Inner {
    config: MigrationConfig,
    statuses: Mutex<BTreeMap<String, ChainMigrationStatus>>,
    queue: Mutex<VecDeque<String>>,
    wake: Condvar,
    outstanding: Mutex<usize>,
    idle: Condvar,
    shutdown: AtomicBool,
}

/// Runs and tracks chain migrations in the background.
///
/// Cheap to clone; all clones share one state. See the [`migration`](crate::migration) module
/// documentation.
#[derive(Clone)]
pub struct MigrationManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MigrationManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationManager")
            .field("chain_dir", &self.inner.config.chain_dir)
            .finish()
    }
}

impl MigrationManager {
    /// Recover interrupted migrations, find chains that need work and start
    /// migrating them in the background. Returns immediately.
    ///
    /// Synchronous work is limited to: the format-stamp check, one journal read
    /// per chain with a journal, and one first-record read per chain. Nothing
    /// is rewritten before this returns.
    ///
    /// # Errors
    ///
    /// [`MigrationError::FutureSchema`] when the directory was written by a
    /// newer release; I/O errors listing the directory.
    pub fn start(config: MigrationConfig) -> Result<Self, MigrationError> {
        check_format_stamp(&config.chain_dir)?;
        fs::create_dir_all(&config.chain_dir)?;
        let workers = config.max_concurrency.max(1);
        let manager = Self {
            inner: Arc::new(Inner {
                config,
                statuses: Mutex::new(BTreeMap::new()),
                queue: Mutex::new(VecDeque::new()),
                wake: Condvar::new(),
                outstanding: Mutex::new(0),
                idle: Condvar::new(),
                shutdown: AtomicBool::new(false),
            }),
        };
        manager.scan_and_enqueue();
        for number in 0..workers {
            let worker = manager.clone();
            std::thread::Builder::new()
                .name(format!("mentisdb-migrate-{number}"))
                .spawn(move || worker.work_loop())
                .map_err(MigrationError::Io)?;
        }
        write_format_stamp(&manager.inner.config.chain_dir)?;
        MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).insert(
            registry_key(&manager.inner.config.chain_dir),
            manager.clone(),
        );
        Ok(manager)
    }

    /// Every chain that has, or had, migration work, ordered by key.
    pub fn statuses(&self) -> Vec<ChainMigrationStatus> {
        self.lock_statuses().values().cloned().collect()
    }

    /// One chain's status, if it has migration work on record.
    pub fn status(&self, chain_key: &str) -> Option<ChainMigrationStatus> {
        self.lock_statuses().get(chain_key).cloned()
    }

    /// True while any chain is queued or running.
    pub fn is_active(&self) -> bool {
        *self
            .inner
            .outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            > 0
    }

    /// Whether `chain_key` can be served, and if not, why.
    ///
    /// `None` means go ahead. This is a map lookup; call it on every request.
    pub fn gate(&self, chain_key: &str) -> Option<MigrationBlock> {
        let status = self.lock_statuses().get(chain_key)?.clone();
        let key = status.chain_key;
        match status.phase {
            MigrationPhase::Completed => None,
            MigrationPhase::Pending | MigrationPhase::Running => Some(MigrationBlock::Migrating {
                chain_key: key,
                percent: status.percent,
                retry_after_secs: 5,
            }),
            MigrationPhase::Failed => Some(MigrationBlock::Quarantined {
                chain_key: key,
                error: status.error.unwrap_or_default(),
            }),
            MigrationPhase::Skipped => Some(MigrationBlock::Skipped { chain_key: key }),
            MigrationPhase::RolledBack => Some(MigrationBlock::RolledBack { chain_key: key }),
        }
    }

    /// Block until no migration is queued or running, or `timeout` elapses.
    /// Returns `true` when idle. Intended for tests and CLI one-shots.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut outstanding = self
            .inner
            .outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        while *outstanding > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (guard, _) = self
                .inner
                .idle
                .wait_timeout(outstanding, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            outstanding = guard;
        }
        true
    }

    /// Clear a quarantine and queue the chain again.
    ///
    /// # Errors
    ///
    /// Whatever [`engine::retry_chain`] refuses.
    pub fn retry(&self, chain_key: &str) -> Result<(), MigrationError> {
        engine::retry_chain(&self.inner.config.chain_dir, chain_key)?;
        self.lock_statuses().remove(chain_key);
        self.consider(chain_key);
        Ok(())
    }

    /// Leave a chain alone and unavailable until `retry`.
    ///
    /// # Errors
    ///
    /// Whatever [`engine::skip_chain`] refuses.
    pub fn skip(&self, chain_key: &str, reason: &str) -> Result<(), MigrationError> {
        engine::skip_chain(&self.inner.config.chain_dir, chain_key, reason)?;
        let mut status = ChainMigrationStatus::new(chain_key, MigrationPhase::Skipped);
        status.error = Some(format!("skipped by the operator: {reason}"));
        self.lock_statuses().insert(chain_key.to_owned(), status);
        Ok(())
    }

    /// Ask the workers to stop after their current chain.
    pub fn shutdown(&self) {
        MANAGERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&registry_key(&self.inner.config.chain_dir));
        self.inner.shutdown.store(true, Ordering::SeqCst);
        self.inner.wake.notify_all();
    }

    fn lock_statuses(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ChainMigrationStatus>> {
        self.inner
            .statuses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn scan_and_enqueue(&self) {
        for key in discover_chain_keys(&self.inner.config.chain_dir) {
            self.consider(&key);
        }
    }

    /// Decide what `key` needs and record it (queueing it when work is due).
    fn consider(&self, key: &str) {
        let dir = &self.inner.config.chain_dir;
        let paths = engine::MigrationPaths::new(dir, key);
        let journal = MigrationJournal::load(&paths.journal).ok().flatten();
        if let Some(journal) = &journal {
            let phase = match journal.state {
                JournalState::Failed => Some(MigrationPhase::Failed),
                JournalState::Skipped => Some(MigrationPhase::Skipped),
                JournalState::RolledBack => Some(MigrationPhase::RolledBack),
                _ => None,
            };
            if let Some(phase) = phase {
                let mut status = ChainMigrationStatus::new(key, phase);
                status.attempts = journal.attempts;
                status.error = journal.error.clone();
                status.reason = journal.reason.map(|r| r.to_string());
                status.finished_at = Some(journal.updated_at);
                status.backup_dir = paths
                    .backup_dir
                    .exists()
                    .then(|| paths.backup_dir.display().to_string());
                self.lock_statuses().insert(key.to_owned(), status);
                return;
            }
            if journal.state == JournalState::Finalized {
                // A finished run is history; still check the chain is current.
                if matches!(engine::detect_migration(dir, key), Ok(None)) {
                    return;
                }
            }
        }
        let interrupted = journal.as_ref().is_some_and(|j| !j.state.is_terminal());
        let needs = if interrupted {
            Ok(journal.as_ref().and_then(|j| j.reason))
        } else {
            engine::detect_migration(dir, key)
        };
        match needs {
            Ok(None) => {}
            Ok(reason) => {
                let mut status = ChainMigrationStatus::new(key, MigrationPhase::Pending);
                status.reason = reason.map(|r| r.to_string());
                status.attempts = journal.as_ref().map(|j| j.attempts).unwrap_or(0);
                self.lock_statuses().insert(key.to_owned(), status);
                self.enqueue(key);
            }
            Err(error) => {
                mig_log!(error, "chain '{key}': cannot be migrated: {error}");
                let mut status = ChainMigrationStatus::new(key, MigrationPhase::Failed);
                status.error = Some(error.to_string());
                status.finished_at = Some(Utc::now());
                self.lock_statuses().insert(key.to_owned(), status);
                // Persist the quarantine so it survives a restart and shows in
                // `migrate status`. A newer-schema chain is never written to.
                if !matches!(error, MigrationError::FutureSchema { .. }) {
                    let _ = engine::quarantine_chain(dir, key, &error.to_string());
                }
            }
        }
    }

    fn enqueue(&self, key: &str) {
        *self
            .inner
            .outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += 1;
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(key.to_owned());
        self.inner.wake.notify_one();
    }

    fn work_loop(&self) {
        loop {
            let key = {
                let mut queue = self.inner.queue.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if self.inner.shutdown.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Some(key) = queue.pop_front() {
                        break key;
                    }
                    queue = self
                        .inner
                        .wake
                        .wait(queue)
                        .unwrap_or_else(|e| e.into_inner());
                }
            };
            self.run_job(&key);
            let mut outstanding = self
                .inner
                .outstanding
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *outstanding = outstanding.saturating_sub(1);
            if *outstanding == 0 {
                self.inner.idle.notify_all();
                drop(outstanding);
                self.prune_old_backups();
            }
        }
    }

    fn run_job(&self, key: &str) {
        let started = Utc::now();
        self.update(key, |status| {
            status.phase = MigrationPhase::Running;
            status.started_at = Some(started);
        });
        let manager = self.clone();
        let observer_key = key.to_owned();
        let user_progress = self.inner.config.options.progress.clone();
        let mut options = self.inner.config.options.clone();
        options.progress = Some(Arc::new(move |progress: &MigrationProgress| {
            manager.update(&observer_key, |status| {
                status.step = Some(progress.step);
                status.percent = progress.percent();
                status.thoughts_done = progress.thoughts_done;
            });
            if let Some(hook) = &user_progress {
                hook(progress);
            }
        }));
        let dir = self.inner.config.chain_dir.clone();
        let key_owned = key.to_owned();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine::run_chain_migration(&dir, &key_owned, &options)
        }));
        let finished = Utc::now();
        let attempts = MigrationJournal::load(&engine::MigrationPaths::new(&dir, key).journal)
            .ok()
            .flatten()
            .map(|j| j.attempts)
            .unwrap_or(0);
        match outcome {
            Ok(Ok(MigrationOutcome::Migrated(report))) => self.update(key, |status| {
                status.phase = MigrationPhase::Completed;
                status.step = None;
                status.percent = 100;
                status.thoughts_done = report.thoughts;
                status.attempts = attempts;
                status.finished_at = Some(finished);
                status.backup_dir = report.backup_dir.as_ref().map(|p| p.display().to_string());
                status.report = Some(report);
            }),
            Ok(Ok(MigrationOutcome::NotNeeded)) => {
                self.lock_statuses().remove(key);
            }
            Ok(Ok(MigrationOutcome::Quarantined { state, error })) => self.update(key, |status| {
                status.phase = match state {
                    JournalState::Skipped => MigrationPhase::Skipped,
                    JournalState::RolledBack => MigrationPhase::RolledBack,
                    _ => MigrationPhase::Failed,
                };
                status.error = error;
                status.finished_at = Some(finished);
            }),
            Ok(Err(MigrationError::Busy { .. })) => {
                // Another process (a CLI run) holds the chain; try again soon.
                std::thread::sleep(Duration::from_secs(2));
                self.enqueue_again(key);
            }
            Ok(Err(error)) => self.update(key, |status| {
                status.phase = MigrationPhase::Failed;
                status.attempts = attempts;
                status.error = Some(error.to_string());
                status.finished_at = Some(finished);
            }),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "migration panicked".to_owned());
                mig_log!(error, "chain '{key}': migration panicked: {message}");
                self.update(key, |status| {
                    status.phase = MigrationPhase::Failed;
                    status.error = Some(format!("migration panicked: {message}"));
                    status.finished_at = Some(finished);
                });
            }
        }
    }

    fn enqueue_again(&self, key: &str) {
        self.inner
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(key.to_owned());
        self.inner.wake.notify_one();
        // The caller releases this job's slot after `run_job` returns; take a
        // new one so the requeued job still counts as outstanding.
        *self
            .inner
            .outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner()) += 1;
    }

    fn update(&self, key: &str, change: impl FnOnce(&mut ChainMigrationStatus)) {
        let mut statuses = self.lock_statuses();
        let status = statuses
            .entry(key.to_owned())
            .or_insert_with(|| ChainMigrationStatus::new(key, MigrationPhase::Pending));
        change(status);
    }

    fn prune_old_backups(&self) {
        if let Some(retention) = self.inner.config.backup_retention {
            if let Ok(removed) =
                engine::purge_backups(&self.inner.config.chain_dir, Some(retention))
            {
                if removed > 0 {
                    mig_log!(
                        info,
                        "removed {removed} migration backup(s) older than {} days",
                        retention.as_secs() / 86_400
                    );
                }
            }
        }
    }
}
