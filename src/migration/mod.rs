//! Chain format migrations: background, journaled, backed up and recoverable.
//!
//! A new mentisdb release may need to rewrite chain files (a newer thought
//! layout, a better hash algorithm, JSONL to binary). Doing that inside
//! `MentisDb::open` or in a synchronous startup pass has three failure modes
//! this module exists to remove:
//!
//! 1. **Startup stalls.** A multi-gigabyte chain rewritten before the daemon
//!    binds its ports looks like a hang. [`MigrationManager::start`] returns in
//!    milliseconds; chains are migrated by a bounded pool of background
//!    workers while the daemon serves everything else. A request for a chain
//!    that is still migrating gets a typed [`MigrationBlock`] (HTTP 503 with
//!    `Retry-After` and the percent done) instead of waiting.
//! 2. **Silent in-place rewrites.** The original bytes are preserved before
//!    anything is replaced, every step is recorded in a durable journal, and a
//!    kill at any instant is resumed or discarded cleanly on the next start
//!    ([`recover_chain`]). [`rollback_chain`] restores the backup.
//! 3. **One bad chain taking everything down.** A chain that fails (corrupt,
//!    tampered, would break a signature, crashed the process repeatedly) is
//!    *quarantined*: reported with its error, left byte-for-byte untouched, and
//!    skipped. Other chains and the daemon are unaffected.
//!
//! # How a migration runs
//!
//! ```text
//! detect ──> Planned ──> BackedUp ──> Staged ──> Committed ──> Finalized
//!   │           │           │           │            │             │
//!   │           │           │           │            │             └ registry + derived files
//!   │           │           │           │            └ atomic rename of the staged file
//!   │           │           │           └ new file fully written, fsynced, re-verified
//!   │           │           └ original hard-linked (or copied) into migrations/backup
//!   │           └ journal written before any work
//!   └ O(1): the first record decides (schema, layout, hash algorithm, JSONL)
//! ```
//!
//! The staged file is produced by *streaming*: records are decoded, upgraded
//! one [`UPGRADE_STEPS`] step at a time, hash-checked and written, so memory
//! stays flat however large the chain is. Hashes are rebuilt only where the
//! old format stored untrustworthy ones; a trustworthy stored hash that does
//! not verify aborts the migration ([`MigrationError::Integrity`]).
//!
//! # Files
//!
//! Everything lives under `<chain_dir>/migrations/` and is safe to inspect:
//!
//! | Path | Purpose |
//! |------|---------|
//! | `journal/<stem>.json` | the per-chain state machine ([`MigrationJournal`]) |
//! | `journal/<stem>.lock` | advisory lock so two processes never migrate one chain |
//! | `staging/<stem>.staged` | the new chain file before the atomic rename |
//! | `backup/<stem>/` | the original chain file and its registries |
//!
//! # Stopping and crashing
//!
//! Two kinds of interruption are told apart. A **clean stop** (the daemon gets
//! `SIGTERM` or Ctrl+C, or the operator quits the TUI) calls
//! [`MigrationManager::shutdown`]: the migration in flight halts at its next
//! record ([`run_chain_migration_cancellable`]), staging is discarded, the live
//! chain is untouched and the attempt is handed back, so restarting a daemon
//! during a long migration any number of times never quarantines the chain. A
//! **crash** (`kill -9`, out of memory, power loss) cannot hand anything back
//! and counts as an attempt; a chain cut off [`MAX_ATTEMPTS`](engine::MAX_ATTEMPTS)
//! (3) times is quarantined so it cannot crash-loop the daemon, and
//! `mentisdb migrate retry` clears it.
//!
//! # Downgrading
//!
//! A migrated chain keeps the current thought layout, so the previous release
//! reads it. A chain written by a *newer* release is refused with
//! [`MigrationError::FutureSchema`] and a data directory stamped by a newer
//! release is refused by [`check_format_stamp`]; neither is ever modified.
//!
//! # Operator commands
//!
//! `mentisdb migrate status|run|verify|retry|skip|rollback|purge-backups`
//! drive this module from the command line; `GET /v1/migrations` and the
//! `mentisdb_migration_status` MCP tool report the same states from a running
//! daemon.

mod codec;
mod engine;
pub mod fixtures;
mod manager;

pub use codec::{
    decode_and_upgrade, decode_json_and_upgrade, peek_schema_version, DecodedRecord, HashRebuilder,
    RecordLayout, UpgradeStep, UPGRADE_STEPS,
};
pub(crate) use engine::preserve_before_rewrite;
pub use engine::{
    detect_migration, dry_run_chain, purge_backups, quarantine_chain, recover_chain, retry_chain,
    rollback_chain, run_chain_migration, run_chain_migration_cancellable, skip_chain,
    verify_chain_file, DryRunReport, JournalState, MigrationJournal, MigrationOutcome,
    MigrationPaths, MigrationReason, MigrationReport, RecoveryOutcome, VerifyReport,
};
pub use manager::{
    check_format_stamp, discover_chain_keys, manager_for_dir, write_format_stamp,
    ChainMigrationStatus, MigrationBlock, MigrationConfig, MigrationManager, MigrationPhase,
    FORMAT_STAMP_FILENAME,
};

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// A point in a migration at which a test can simulate the process being killed.
///
/// With a hook installed in [`MigrationOptions::failpoint`], returning `true`
/// for a failpoint makes the run stop *immediately* with
/// [`MigrationError::Injected`] and without any cleanup, leaving the disk
/// exactly as a `kill -9` at that instant would. The recovery matrix in the
/// test suite runs every variant.
///
/// # Examples
///
/// ```
/// use mentisdb::migration::Failpoint;
///
/// let all = Failpoint::all();
/// assert!(all.contains(&Failpoint::BeforeCommitRename));
/// assert_eq!(Failpoint::DuringStage { thought: 7 }.to_string(), "during-stage@7");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Failpoint {
    /// Journal written as `Planned`, nothing else done.
    AfterPlan,
    /// Original preserved, journal `BackedUp`, staging not started.
    AfterBackup,
    /// Part-way through writing the staged file, before record `thought`.
    DuringStage {
        /// Zero-based index of the next record to be written.
        thought: u64,
    },
    /// Staged file complete and fsynced, journal not yet `Staged`.
    AfterStage,
    /// Journal `Staged`, live file still the original.
    BeforeCommitRename,
    /// Staged file renamed over the live path, journal still `Staged`.
    AfterCommitRename,
    /// Journal `Committed`, registry and derived files untouched.
    AfterCommitJournal,
    /// About to update the registry and derived files.
    BeforeFinalize,
}

impl Failpoint {
    /// Every failpoint, with `DuringStage` at its first and a later record.
    pub fn all() -> Vec<Failpoint> {
        vec![
            Failpoint::AfterPlan,
            Failpoint::AfterBackup,
            Failpoint::DuringStage { thought: 0 },
            Failpoint::DuringStage { thought: 3 },
            Failpoint::AfterStage,
            Failpoint::BeforeCommitRename,
            Failpoint::AfterCommitRename,
            Failpoint::AfterCommitJournal,
            Failpoint::BeforeFinalize,
        ]
    }
}

impl fmt::Display for Failpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AfterPlan => f.write_str("after-plan"),
            Self::AfterBackup => f.write_str("after-backup"),
            Self::DuringStage { thought } => write!(f, "during-stage@{thought}"),
            Self::AfterStage => f.write_str("after-stage"),
            Self::BeforeCommitRename => f.write_str("before-commit-rename"),
            Self::AfterCommitRename => f.write_str("after-commit-rename"),
            Self::AfterCommitJournal => f.write_str("after-commit-journal"),
            Self::BeforeFinalize => f.write_str("before-finalize"),
        }
    }
}

/// Test hook: return `true` to simulate a kill at the given [`Failpoint`].
pub type FailpointHook = Arc<dyn Fn(&Failpoint) -> bool + Send + Sync>;

/// Why a migration did not complete.
///
/// Every variant other than [`Self::Injected`], [`Self::Busy`] and [`Self::Interrupted`] quarantines
/// the chain (journal state `Failed`) with the error text preserved.
#[derive(Debug)]
pub enum MigrationError {
    /// Filesystem failure.
    Io(std::io::Error),
    /// A record, file or journal could not be decoded.
    Corrupt {
        /// Zero-based record index when known.
        index: Option<u64>,
        /// What was wrong.
        message: String,
    },
    /// A trustworthy stored hash does not verify, or indexes are out of order.
    Integrity {
        /// Zero-based index of the offending record.
        index: u64,
        /// What was wrong.
        message: String,
    },
    /// The chain was written by a newer mentisdb than this build.
    FutureSchema {
        /// The `schema_version` found.
        version: u32,
    },
    /// Rebuilding hashes would invalidate a signature bound to the old head.
    SignatureWouldBreak {
        /// Zero-based index of the signed record.
        index: u64,
    },
    /// Another process or thread holds this chain's migration lock.
    Busy {
        /// The chain being migrated elsewhere.
        chain_key: String,
    },
    /// A request that the current state does not allow (for example rolling
    /// back after new thoughts were appended).
    Refused(String),
    /// A test failpoint fired; the disk was deliberately left mid-flight.
    Injected(Failpoint),
    /// The run was asked to stop ([`run_chain_migration_cancellable`]). Not a
    /// failure: the live chain is untouched and the attempt is handed back.
    /// [`run_chain_migration_cancellable`] turns it into
    /// [`MigrationOutcome::Interrupted`], so callers of the public entry points
    /// do not normally see it.
    Interrupted,
}

impl MigrationError {
    /// True for [`Self::Injected`], i.e. a simulated crash.
    pub fn is_injected(&self) -> bool {
        matches!(self, Self::Injected(_))
    }
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Corrupt {
                index: Some(index),
                message,
            } => write!(f, "corrupt record {index}: {message}"),
            Self::Corrupt {
                index: None,
                message,
            } => write!(f, "corrupt chain data: {message}"),
            Self::Integrity { index, message } => {
                write!(f, "integrity failure at thought {index}: {message}")
            }
            Self::FutureSchema { version } => write!(
                f,
                "chain uses schema {version}, newer than this mentisdb supports (schema {}); \
                 upgrade mentisdb to open it",
                crate::MENTISDB_CURRENT_VERSION
            ),
            Self::SignatureWouldBreak { index } => write!(
                f,
                "migrating would invalidate the signature on thought {index}; \
                 the chain was left untouched"
            ),
            Self::Busy { chain_key } => {
                write!(
                    f,
                    "chain '{chain_key}' is being migrated by another process"
                )
            }
            Self::Refused(message) => f.write_str(message),
            Self::Injected(failpoint) => write!(f, "injected crash at {failpoint}"),
            Self::Interrupted => f.write_str("migration stopped on request"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<std::io::Error> for MigrationError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<MigrationError> for std::io::Error {
    fn from(error: MigrationError) -> Self {
        match error {
            MigrationError::Io(error) => error,
            other => std::io::Error::new(std::io::ErrorKind::InvalidData, other.to_string()),
        }
    }
}

/// Coarse position of a running migration, reported to progress observers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStep {
    /// Looking at the chain to decide what, if anything, to do.
    Planning,
    /// Preserving the original files.
    BackingUp,
    /// Streaming the upgraded chain into the staging file.
    Staging,
    /// Re-reading the staged file and checking it end to end.
    Verifying,
    /// Swapping the staged file into place.
    Committing,
    /// Updating the registry and invalidating derived files.
    Finalizing,
}

impl fmt::Display for MigrationStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Planning => "planning",
            Self::BackingUp => "backing-up",
            Self::Staging => "staging",
            Self::Verifying => "verifying",
            Self::Committing => "committing",
            Self::Finalizing => "finalizing",
        })
    }
}

/// A progress sample passed to [`MigrationOptions::progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationProgress {
    /// The chain being migrated.
    pub chain_key: String,
    /// Where the run is.
    pub step: MigrationStep,
    /// Records written so far.
    pub thoughts_done: u64,
    /// Source bytes consumed so far.
    pub bytes_done: u64,
    /// Size of the source file.
    pub bytes_total: u64,
}

impl MigrationProgress {
    /// Whole-percent progress through the staging pass (0 to 100).
    pub fn percent(&self) -> u8 {
        if self.bytes_total == 0 {
            return 0;
        }
        ((self.bytes_done.saturating_mul(100)) / self.bytes_total).min(100) as u8
    }
}

/// Progress observer type.
pub type ProgressHook = Arc<dyn Fn(&MigrationProgress) + Send + Sync>;

/// Knobs for one engine run.
///
/// # Examples
///
/// ```
/// use mentisdb::migration::MigrationOptions;
///
/// let options = MigrationOptions::default();
/// assert!(options.failpoint.is_none() && options.progress.is_none());
/// ```
#[derive(Clone, Default)]
pub struct MigrationOptions {
    /// Simulated-crash hook, tests only.
    pub failpoint: Option<FailpointHook>,
    /// Called every few thousand records and at each step boundary.
    pub progress: Option<ProgressHook>,
}

impl fmt::Debug for MigrationOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MigrationOptions")
            .field("failpoint", &self.failpoint.is_some())
            .field("progress", &self.progress.is_some())
            .finish()
    }
}

impl MigrationOptions {
    /// Options that simulate a kill at `failpoint` and nothing else.
    pub fn crash_at(failpoint: Failpoint) -> Self {
        Self {
            failpoint: Some(Arc::new(move |hit| *hit == failpoint)),
            progress: None,
        }
    }

    pub(crate) fn check(&self, failpoint: Failpoint) -> Result<(), MigrationError> {
        match &self.failpoint {
            Some(hook) if hook(&failpoint) => Err(MigrationError::Injected(failpoint)),
            _ => Ok(()),
        }
    }

    pub(crate) fn report(&self, progress: MigrationProgress) {
        if let Some(hook) = &self.progress {
            hook(&progress);
        }
    }
}

/// Log through `log` when the server feature is on, `stderr` otherwise.
macro_rules! mig_log {
    ($level:ident, $($arg:tt)*) => {{
        #[cfg(feature = "server")]
        log::$level!("[migration] {}", format!($($arg)*));
        #[cfg(not(feature = "server"))]
        eprintln!("[mentisdb migration] {}", format!($($arg)*));
    }};
}
pub(crate) use mig_log;
