//! The per-chain migration engine: detect, back up, stage, verify, commit,
//! finalize, with a durable journal that makes every step resumable.
//!
//! All entry points take the chain directory and chain key and are safe to
//! call from any thread or process: a per-chain advisory lock
//! ([`MigrationError::Busy`]) keeps two migrators off one chain. See the
//! [module documentation](super) for the state machine and file layout.

use super::codec::{decode_and_upgrade, decode_json_and_upgrade, HashRebuilder, RecordLayout};
use super::{
    mig_log, Failpoint, MigrationError, MigrationOptions, MigrationProgress, MigrationStep,
};
use crate::{
    chain_storage_filename, compute_thought_hash, read_next_binary_payload, StorageAdapterKind,
    Thought, MENTISDB_CURRENT_VERSION,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A chain interrupted this many times is quarantined instead of retried, so a
/// chain that crashes the process (out of memory, a decoder bug) cannot turn
/// every start into a crash loop.
pub const MAX_ATTEMPTS: u32 = 3;

/// Records between progress samples while staging.
const PROGRESS_EVERY: u64 = 2048;

/// Why a chain needs migrating, decided from its first record alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "version")]
pub enum MigrationReason {
    /// The first record is from schema `version` (0 to 2).
    LegacySchema(u32),
    /// Schema 3 written before `entity_type` and `source_episode` existed.
    LegacyLayout,
    /// Current layout, but hashed with the legacy JSON algorithm.
    LegacyHash,
    /// Current layout with empty hash fields.
    MissingHash,
    /// The chain is a legacy JSONL file; chains are binary now.
    JsonlStorage,
}

impl std::fmt::Display for MigrationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacySchema(version) => write!(f, "legacy schema {version}"),
            Self::LegacyLayout => f.write_str("legacy schema-3 layout"),
            Self::LegacyHash => f.write_str("legacy JSON hash algorithm"),
            Self::MissingHash => f.write_str("missing hashes"),
            Self::JsonlStorage => f.write_str("legacy JSONL storage"),
        }
    }
}

/// Where a chain is in its migration, as persisted in the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalState {
    /// Journal written; nothing else done.
    Planned,
    /// Original preserved.
    BackedUp,
    /// Staged file complete and verified; live file still the original.
    Staged,
    /// Staged file is the live file.
    Committed,
    /// Registry updated and derived files invalidated. Terminal, success.
    Finalized,
    /// Restored from backup. Terminal.
    RolledBack,
    /// Quarantined with an error. Terminal until [`retry_chain`].
    Failed,
    /// Left alone on the operator's request. Terminal until [`retry_chain`].
    Skipped,
}

impl JournalState {
    /// True for states that need no recovery.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::RolledBack | Self::Failed | Self::Skipped
        )
    }
}

/// The durable record of one chain's migration.
///
/// Rewritten atomically (temp file, fsync, rename, directory fsync) at every
/// state change, so after any crash it describes a state the disk is really in
/// or one step behind it, never ahead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationJournal {
    /// Journal format version.
    pub format: u32,
    /// The chain being migrated.
    pub chain_key: String,
    /// Where the run is.
    pub state: JournalState,
    /// Why the chain needed migrating.
    pub reason: Option<MigrationReason>,
    /// File name of the original chain file.
    pub source_file: String,
    /// Storage kind of the original.
    pub source_kind: StorageAdapterKind,
    /// How many times a run has started (resumes included).
    pub attempts: u32,
    /// Records in the new chain, once staged.
    pub thoughts_total: u64,
    /// Length of the staged file.
    pub staged_len: u64,
    /// SHA-256 of the staged file, hex.
    pub staged_digest: String,
    /// Head hash of the new chain, once staged.
    pub new_head_hash: String,
    /// Whether any hash changed (derived files are then invalidated).
    pub hash_changed: bool,
    /// Registry entry before the migration, restored on rollback.
    pub original_registration: Option<crate::MentisDbRegistration>,
    /// Last error text, for `Failed`.
    pub error: Option<String>,
    /// Process that last wrote the journal.
    pub pid: u32,
    /// When the journal was first written.
    pub started_at: DateTime<Utc>,
    /// When it was last written.
    pub updated_at: DateTime<Utc>,
}

/// Where everything for one chain's migration lives.
///
/// # Examples
///
/// ```
/// use mentisdb::migration::MigrationPaths;
///
/// let paths = MigrationPaths::new(std::path::Path::new("/data"), "my-chain");
/// assert!(paths.journal.starts_with("/data/migrations/journal"));
/// assert!(paths.live.to_string_lossy().ends_with(".tcbin"));
/// ```
#[derive(Debug, Clone)]
pub struct MigrationPaths {
    /// Storage directory.
    pub chain_dir: PathBuf,
    /// Chain key.
    pub chain_key: String,
    /// Where the binary chain lives (the migration target).
    pub live: PathBuf,
    /// Where a legacy JSONL chain would live.
    pub legacy_jsonl: PathBuf,
    /// The journal file.
    pub journal: PathBuf,
    /// The lock file.
    pub lock: PathBuf,
    /// The staged file.
    pub staged: PathBuf,
    /// Directory holding the preserved original.
    pub backup_dir: PathBuf,
}

impl MigrationPaths {
    /// Compute every path for `chain_key` under `chain_dir`.
    pub fn new(chain_dir: &Path, chain_key: &str) -> Self {
        let binary = chain_storage_filename(chain_key, StorageAdapterKind::Binary);
        let stem = binary
            .strip_suffix(&format!(".{}", StorageAdapterKind::Binary.file_extension()))
            .unwrap_or(&binary)
            .to_owned();
        let root = chain_dir.join("migrations");
        Self {
            chain_dir: chain_dir.to_path_buf(),
            chain_key: chain_key.to_owned(),
            live: chain_dir.join(&binary),
            legacy_jsonl: chain_dir
                .join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl)),
            journal: root.join("journal").join(format!("{stem}.json")),
            lock: root.join("journal").join(format!("{stem}.lock")),
            staged: root.join("staging").join(format!("{stem}.staged")),
            backup_dir: root.join("backup").join(&stem),
        }
    }

    fn source(&self, kind: StorageAdapterKind) -> &Path {
        match kind {
            StorageAdapterKind::Binary => &self.live,
            StorageAdapterKind::Jsonl => &self.legacy_jsonl,
        }
    }
}

/// What [`run_chain_migration`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The chain is already in the current format.
    NotNeeded,
    /// The chain was migrated; details inside.
    Migrated(MigrationReport),
    /// The chain is quarantined (`Failed`, `Skipped` or `RolledBack`) and was not touched.
    Quarantined {
        /// The journal state that blocked the run.
        state: JournalState,
        /// The recorded error, if any.
        error: Option<String>,
    },
}

/// Summary of a completed (or dry-run) migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    /// The chain.
    pub chain_key: String,
    /// Why it needed migrating.
    pub reason: Option<MigrationReason>,
    /// Oldest record schema found.
    pub from_schema: u32,
    /// Schema written.
    pub to_schema: u32,
    /// Records migrated.
    pub thoughts: u64,
    /// Source bytes read.
    pub source_bytes: u64,
    /// New file size.
    pub staged_bytes: u64,
    /// Names of the upgrade steps that ran.
    pub steps: Vec<String>,
    /// True when any hash changed.
    pub hash_changed: bool,
    /// Signed thoughts in the chain.
    pub signed: u64,
    /// Bytes of an incomplete final record dropped (kept in the backup).
    pub torn_tail_bytes: u64,
    /// Wall-clock milliseconds the staging pass took.
    pub elapsed_ms: u64,
    /// Where the original was preserved.
    pub backup_dir: Option<PathBuf>,
    /// True when this run resumed an interrupted one.
    pub resumed: bool,
}

/// Result of a read-only migration rehearsal; see [`dry_run_chain`].
pub type DryRunReport = MigrationReport;

/// Result of [`verify_chain_file`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    /// Records read.
    pub thoughts: u64,
    /// Signed records.
    pub signed: u64,
    /// Hash of the last record.
    pub head_hash: String,
    /// File size.
    pub file_len: u64,
}

/// What [`recover_chain`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No journal, or a terminal one: nothing to recover.
    NothingToDo,
    /// An interrupted run was completed (commit and finalize).
    Completed,
    /// An interrupted run left the original intact; staging was discarded and
    /// the migration must run again.
    Restart,
    /// The chain was quarantined (too many interruptions or an unusable stage).
    Quarantined,
}

// ---------------------------------------------------------------------------
// durable file helpers
// ---------------------------------------------------------------------------

/// fsync a directory so a rename or unlink inside it survives power loss.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn write_json_durable<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{}", std::process::id()));
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = File::create(&temp)?;
        serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    sync_dir(parent);
    Ok(())
}

/// Counts and hashes everything written through it.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Counts bytes read through it.
struct CountingReader<R: Read> {
    inner: R,
    read: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

fn file_digest(path: &Path) -> io::Result<(u64, String)> {
    let mut file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buf = vec![0_u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((total, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Exclusive per-chain advisory lock, released on drop.
struct ChainLock {
    _file: File,
}

impl ChainLock {
    fn acquire(paths: &MigrationPaths) -> Result<Self, MigrationError> {
        if let Some(parent) = paths.lock.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&paths.lock)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(MigrationError::Busy {
                chain_key: paths.chain_key.clone(),
            }),
            Err(fs::TryLockError::Error(error)) => Err(MigrationError::Io(error)),
        }
    }
}

// ---------------------------------------------------------------------------
// journal persistence
// ---------------------------------------------------------------------------

impl MigrationJournal {
    fn new(
        paths: &MigrationPaths,
        kind: StorageAdapterKind,
        reason: Option<MigrationReason>,
    ) -> Self {
        let now = Utc::now();
        Self {
            format: 1,
            chain_key: paths.chain_key.clone(),
            state: JournalState::Planned,
            reason,
            source_file: paths
                .source(kind)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            source_kind: kind,
            attempts: 0,
            thoughts_total: 0,
            staged_len: 0,
            staged_digest: String::new(),
            new_head_hash: String::new(),
            hash_changed: false,
            original_registration: None,
            error: None,
            pid: std::process::id(),
            started_at: now,
            updated_at: now,
        }
    }

    /// Read the journal at `path`.
    ///
    /// # Errors
    ///
    /// [`MigrationError::Corrupt`] when the file exists but is not a journal.
    pub fn load(path: &Path) -> Result<Option<Self>, MigrationError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| MigrationError::Corrupt {
                index: None,
                message: format!("unreadable migration journal {}: {error}", path.display()),
            })
    }

    fn save(&mut self, paths: &MigrationPaths) -> Result<(), MigrationError> {
        self.pid = std::process::id();
        self.updated_at = Utc::now();
        write_json_durable(&paths.journal, self)?;
        Ok(())
    }

    fn set_state(
        &mut self,
        paths: &MigrationPaths,
        state: JournalState,
    ) -> Result<(), MigrationError> {
        self.state = state;
        self.save(paths)
    }
}

/// Every journal under `chain_dir`, for status listings and startup recovery.
pub(crate) fn list_journals(chain_dir: &Path) -> Vec<MigrationJournal> {
    let dir = chain_dir.join("migrations").join("journal");
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut journals: Vec<MigrationJournal> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| MigrationJournal::load(&entry.path()).ok().flatten())
        .collect();
    journals.sort_by(|a, b| a.chain_key.cmp(&b.chain_key));
    journals
}

// ---------------------------------------------------------------------------
// detection
// ---------------------------------------------------------------------------

/// Decide from the first record alone whether `chain_key` needs migrating.
///
/// O(1) in the chain size: schema versions and hash algorithms only ever move
/// forward along a chain, so the first record bounds everything after it.
/// Returns `Ok(None)` for a chain that is current, empty or absent.
///
/// # Errors
///
/// [`MigrationError::FutureSchema`] for a chain from a newer release,
/// [`MigrationError::Integrity`] when the first record's hash is wrong under
/// every known algorithm, [`MigrationError::Corrupt`] when it cannot be
/// decoded, and `Corrupt` when both a JSONL and a binary file exist.
pub fn detect_migration(
    chain_dir: &Path,
    chain_key: &str,
) -> Result<Option<MigrationReason>, MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    detect_with_kind(&paths).map(|found| found.map(|(_, reason)| reason))
}

fn detect_with_kind(
    paths: &MigrationPaths,
) -> Result<Option<(StorageAdapterKind, MigrationReason)>, MigrationError> {
    match (paths.live.exists(), paths.legacy_jsonl.exists()) {
        (false, false) => Ok(None),
        (false, true) => Ok(Some((
            StorageAdapterKind::Jsonl,
            MigrationReason::JsonlStorage,
        ))),
        (true, true) => Err(MigrationError::Corrupt {
            index: None,
            message: format!(
                "both {} and {} exist; resolve the conflict before migrating",
                paths.live.display(),
                paths.legacy_jsonl.display()
            ),
        }),
        (true, false) => {
            let mut file = File::open(&paths.live)?;
            let Some(payload) = read_next_binary_payload(&mut file)? else {
                return Ok(None);
            };
            let decoded = decode_and_upgrade(&payload)?;
            let reason = match decoded.layout {
                RecordLayout::Schema0To1 => Some(MigrationReason::LegacySchema(
                    super::codec::peek_schema_version(&payload)?,
                )),
                RecordLayout::Schema2 => Some(MigrationReason::LegacySchema(2)),
                RecordLayout::Schema3Base => Some(MigrationReason::LegacyLayout),
                RecordLayout::Current => {
                    let thought = decoded.thought;
                    if thought.hash.is_empty() {
                        Some(MigrationReason::MissingHash)
                    } else if thought.hash == compute_thought_hash(&thought) {
                        None
                    } else if thought.hash == crate::compute_thought_hash_legacy(&thought) {
                        Some(MigrationReason::LegacyHash)
                    } else {
                        return Err(MigrationError::Integrity {
                            index: 0,
                            message: "the first thought's hash is invalid under every known \
                                      algorithm"
                                .to_string(),
                        });
                    }
                }
            };
            Ok(reason.map(|reason| (StorageAdapterKind::Binary, reason)))
        }
    }
}

// ---------------------------------------------------------------------------
// streaming upgrade
// ---------------------------------------------------------------------------

struct StageSummary {
    thoughts: u64,
    source_bytes: u64,
    torn_tail_bytes: u64,
    head_hash: String,
    hash_changed: bool,
    signed: u64,
    steps: BTreeSet<&'static str>,
    from_schema: u32,
}

/// One decoded source record, whatever the source format.
fn for_each_record(
    path: &Path,
    kind: StorageAdapterKind,
    bytes_read: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut visit: impl FnMut(Thought, RecordLayout, Vec<&'static str>, u64) -> Result<(), MigrationError>,
) -> Result<u64, MigrationError> {
    let file = File::open(path)?;
    let reader = CountingReader {
        inner: file,
        read: std::sync::Arc::clone(bytes_read),
    };
    let mut reader = BufReader::with_capacity(1 << 20, reader);
    let mut consumed = 0_u64;
    let mut index = 0_u64;
    match kind {
        StorageAdapterKind::Binary => {
            while let Some(payload) = read_next_binary_payload(&mut reader)? {
                consumed += 8 + payload.len() as u64;
                let decoded = decode_and_upgrade(&payload).map_err(|error| match error {
                    MigrationError::Corrupt {
                        index: None,
                        message,
                    } => MigrationError::Corrupt {
                        index: Some(index),
                        message,
                    },
                    other => other,
                })?;
                visit(decoded.thought, decoded.layout, decoded.steps, index)?;
                index += 1;
            }
        }
        StorageAdapterKind::Jsonl => {
            for line in reader.lines() {
                let line = line?;
                consumed += line.len() as u64 + 1;
                if line.trim().is_empty() {
                    continue;
                }
                let decoded = decode_json_and_upgrade(&line).map_err(|error| match error {
                    MigrationError::Corrupt {
                        index: None,
                        message,
                    } => MigrationError::Corrupt {
                        index: Some(index),
                        message,
                    },
                    other => other,
                })?;
                visit(decoded.thought, decoded.layout, decoded.steps, index)?;
                index += 1;
            }
        }
    }
    Ok(consumed)
}

fn stream_upgrade<W: Write>(
    paths: &MigrationPaths,
    kind: StorageAdapterKind,
    out: &mut W,
    options: &MigrationOptions,
) -> Result<StageSummary, MigrationError> {
    let source = paths.source(kind);
    let source_len = fs::metadata(source)?.len();
    let bytes_read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut rebuilder = HashRebuilder::default();
    let mut steps = BTreeSet::new();
    let mut from_schema = MENTISDB_CURRENT_VERSION;
    let mut thoughts = 0_u64;
    let progress_bytes = std::sync::Arc::clone(&bytes_read);
    let consumed = for_each_record(
        source,
        kind,
        &bytes_read,
        |thought, layout, applied, index| {
            options.check(Failpoint::DuringStage { thought: index })?;
            from_schema = from_schema.min(layout.schema_version());
            steps.extend(applied);
            let upgraded = rebuilder.apply(thought, layout)?;
            let payload = bincode::serde::encode_to_vec(&upgraded, bincode::config::standard())
                .map_err(|error| MigrationError::Io(io::Error::other(error.to_string())))?;
            out.write_all(&(payload.len() as u64).to_le_bytes())?;
            out.write_all(&payload)?;
            thoughts += 1;
            if thoughts.is_multiple_of(PROGRESS_EVERY) {
                options.report(MigrationProgress {
                    chain_key: paths.chain_key.clone(),
                    step: MigrationStep::Staging,
                    thoughts_done: thoughts,
                    bytes_done: progress_bytes.load(std::sync::atomic::Ordering::Relaxed),
                    bytes_total: source_len,
                });
            }
            Ok(())
        },
    )?;
    options.report(MigrationProgress {
        chain_key: paths.chain_key.clone(),
        step: MigrationStep::Staging,
        thoughts_done: thoughts,
        bytes_done: source_len,
        bytes_total: source_len,
    });
    let torn_tail_bytes = match kind {
        StorageAdapterKind::Binary => source_len.saturating_sub(consumed),
        StorageAdapterKind::Jsonl => 0,
    };
    Ok(StageSummary {
        thoughts,
        source_bytes: source_len,
        torn_tail_bytes,
        head_hash: rebuilder.head_hash().to_owned(),
        hash_changed: rebuilder.hash_changed,
        signed: rebuilder.signed,
        steps,
        from_schema: if thoughts == 0 {
            MENTISDB_CURRENT_VERSION
        } else {
            from_schema
        },
    })
}

/// Re-read a binary chain file and check it end to end under the current rules.
///
/// Every record must decode in the current layout, indexes must run 0, 1, 2,
/// each `prev_hash` must equal the previous record's hash, and every hash must
/// equal the recomputed current-algorithm hash. This is what the engine runs
/// over the staged file before committing it, and what `mentisdb migrate
/// verify` runs over a live chain.
///
/// # Errors
///
/// [`MigrationError::Integrity`] or [`MigrationError::Corrupt`] at the first
/// bad record.
pub fn verify_chain_file(path: &Path) -> Result<VerifyReport, MigrationError> {
    let file_len = fs::metadata(path)?.len();
    let mut reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut previous = String::new();
    let mut thoughts = 0_u64;
    let mut signed = 0_u64;
    while let Some(payload) = read_next_binary_payload(&mut reader)? {
        let decoded = decode_and_upgrade(&payload)?;
        let thought = decoded.thought;
        if decoded.layout != RecordLayout::Current {
            return Err(MigrationError::Corrupt {
                index: Some(thoughts),
                message: format!("record is still in a legacy layout ({:?})", decoded.layout),
            });
        }
        let fail = |message: &str| MigrationError::Integrity {
            index: thoughts,
            message: message.to_owned(),
        };
        if thought.index != thoughts {
            return Err(fail("index out of sequence"));
        }
        if thought.prev_hash != previous {
            return Err(fail("prev_hash does not link to the previous record"));
        }
        if thought.hash != compute_thought_hash(&thought) {
            return Err(fail("hash does not match the record"));
        }
        if thought.thought_signature.is_some() {
            signed += 1;
        }
        previous = thought.hash;
        thoughts += 1;
    }
    Ok(VerifyReport {
        thoughts,
        signed,
        head_hash: previous,
        file_len,
    })
}

// ---------------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------------

/// Migrate one chain to the current format, resuming an interrupted run first.
///
/// Returns [`MigrationOutcome::NotNeeded`] for a current chain, so calling this
/// on every chain at every start is cheap and safe. On any failure other than a
/// simulated crash the journal is marked `Failed`, the staging file is removed,
/// the live chain is left exactly as it was, and the error is returned.
///
/// # Errors
///
/// Any [`MigrationError`]; [`MigrationError::Busy`] when another process holds
/// the chain's lock.
///
/// # Examples
///
/// ```
/// use mentisdb::migration::{run_chain_migration, MigrationOptions, MigrationOutcome};
///
/// let dir = tempfile::tempdir().unwrap();
/// // A chain that does not exist (or is current) needs nothing.
/// let outcome = run_chain_migration(dir.path(), "demo", &MigrationOptions::default()).unwrap();
/// assert_eq!(outcome, MigrationOutcome::NotNeeded);
/// ```
pub fn run_chain_migration(
    chain_dir: &Path,
    chain_key: &str,
    options: &MigrationOptions,
) -> Result<MigrationOutcome, MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    let existing = MigrationJournal::load(&paths.journal)?;
    let mut resumed = false;
    let mut attempts = 0;
    if let Some(journal) = existing {
        match journal.state {
            JournalState::Failed | JournalState::Skipped | JournalState::RolledBack => {
                return Ok(MigrationOutcome::Quarantined {
                    state: journal.state,
                    error: journal.error,
                });
            }
            JournalState::Finalized => {}
            _ => {
                attempts = journal.attempts;
                match recover_locked(&paths, journal, options)? {
                    RecoveryOutcome::Completed => {
                        return Ok(MigrationOutcome::Migrated(report_from_journal(&paths)?));
                    }
                    RecoveryOutcome::Quarantined => {
                        let journal = MigrationJournal::load(&paths.journal)?;
                        return Ok(MigrationOutcome::Quarantined {
                            state: JournalState::Failed,
                            error: journal.and_then(|j| j.error),
                        });
                    }
                    RecoveryOutcome::Restart | RecoveryOutcome::NothingToDo => resumed = true,
                }
            }
        }
    }

    let (kind, reason) = match detect_with_kind(&paths) {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(MigrationOutcome::NotNeeded),
        Err(error) => return quarantine(&paths, None, StorageAdapterKind::Binary, error, attempts),
    };

    let mut journal = MigrationJournal::new(&paths, kind, Some(reason));
    journal.attempts = attempts + 1;
    journal.original_registration = crate::load_registered_chains(chain_dir)
        .ok()
        .and_then(|registry| registry.chains.get(chain_key).cloned());
    mig_log!(
        info,
        "chain '{chain_key}': migrating ({reason}), attempt {}",
        journal.attempts
    );
    journal.save(&paths)?;
    options.check(Failpoint::AfterPlan)?;

    match execute(&paths, &mut journal, kind, options, resumed) {
        Ok(report) => Ok(MigrationOutcome::Migrated(report)),
        Err(error) if error.is_injected() => Err(error),
        Err(error) => quarantine(&paths, Some(journal), kind, error, attempts + 1),
    }
}

/// Record a failure: journal `Failed`, staging removed, live chain untouched.
fn quarantine(
    paths: &MigrationPaths,
    journal: Option<MigrationJournal>,
    kind: StorageAdapterKind,
    error: MigrationError,
    attempts: u32,
) -> Result<MigrationOutcome, MigrationError> {
    mig_log!(
        error,
        "chain '{}': migration failed: {error}",
        paths.chain_key
    );
    let _ = fs::remove_file(&paths.staged);
    let mut journal = journal.unwrap_or_else(|| MigrationJournal::new(paths, kind, None));
    journal.attempts = journal.attempts.max(attempts);
    journal.error = Some(error.to_string());
    let _ = journal.set_state(paths, JournalState::Failed);
    Err(error)
}

fn execute(
    paths: &MigrationPaths,
    journal: &mut MigrationJournal,
    kind: StorageAdapterKind,
    options: &MigrationOptions,
    resumed: bool,
) -> Result<MigrationReport, MigrationError> {
    options.report(progress_marker(paths, MigrationStep::BackingUp));
    backup_original(paths, kind)?;
    journal.set_state(paths, JournalState::BackedUp)?;
    options.check(Failpoint::AfterBackup)?;

    fs::create_dir_all(paths.staged.parent().unwrap_or(&paths.chain_dir))?;
    let started = Instant::now();
    let mut writer = HashingWriter::new(BufWriter::with_capacity(
        1 << 20,
        File::create(&paths.staged)?,
    ));
    let summary = stream_upgrade(paths, kind, &mut writer, options)?;
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    let staged_len = writer.written;
    let staged_digest = hex(&writer.hasher.clone().finalize());
    drop(writer);
    options.check(Failpoint::AfterStage)?;

    options.report(progress_marker(paths, MigrationStep::Verifying));
    let verified = verify_chain_file(&paths.staged)?;
    if verified.thoughts != summary.thoughts || verified.head_hash != summary.head_hash {
        return Err(MigrationError::Integrity {
            index: verified.thoughts,
            message: "the staged chain does not match what was written".to_string(),
        });
    }
    journal.thoughts_total = summary.thoughts;
    journal.staged_len = staged_len;
    journal.staged_digest = staged_digest;
    journal.new_head_hash = summary.head_hash.clone();
    journal.hash_changed = summary.hash_changed;
    journal.set_state(paths, JournalState::Staged)?;
    options.check(Failpoint::BeforeCommitRename)?;

    options.report(progress_marker(paths, MigrationStep::Committing));
    fs::rename(&paths.staged, &paths.live)?;
    sync_dir(&paths.chain_dir);
    options.check(Failpoint::AfterCommitRename)?;
    journal.set_state(paths, JournalState::Committed)?;
    options.check(Failpoint::AfterCommitJournal)?;

    finalize(paths, journal, options)?;
    mig_log!(
        info,
        "chain '{}': migrated {} thoughts in {} ms",
        paths.chain_key,
        summary.thoughts,
        started.elapsed().as_millis()
    );
    Ok(MigrationReport {
        chain_key: paths.chain_key.clone(),
        reason: journal.reason,
        from_schema: summary.from_schema,
        to_schema: MENTISDB_CURRENT_VERSION,
        thoughts: summary.thoughts,
        source_bytes: summary.source_bytes,
        staged_bytes: staged_len,
        steps: summary.steps.iter().map(|s| (*s).to_owned()).collect(),
        hash_changed: summary.hash_changed,
        signed: summary.signed,
        torn_tail_bytes: summary.torn_tail_bytes,
        elapsed_ms: started.elapsed().as_millis() as u64,
        backup_dir: Some(paths.backup_dir.clone()),
        resumed,
    })
}

fn progress_marker(paths: &MigrationPaths, step: MigrationStep) -> MigrationProgress {
    MigrationProgress {
        chain_key: paths.chain_key.clone(),
        step,
        thoughts_done: 0,
        bytes_done: 0,
        bytes_total: 0,
    }
}

/// Preserve the original chain file: hard link (free, and a later rename never
/// touches it) with a copy as the fallback.
fn backup_original(paths: &MigrationPaths, kind: StorageAdapterKind) -> Result<(), MigrationError> {
    let source = paths.source(kind);
    if paths.backup_dir.exists() {
        fs::remove_dir_all(&paths.backup_dir)?;
    }
    fs::create_dir_all(&paths.backup_dir)?;
    let target = paths
        .backup_dir
        .join(source.file_name().unwrap_or_default());
    if fs::hard_link(source, &target).is_err() {
        fs::copy(source, &target)?;
        File::open(&target)?.sync_all()?;
    }
    sync_dir(&paths.backup_dir);
    Ok(())
}

/// Commit bookkeeping that is safe to repeat: remove the superseded JSONL,
/// refresh the registry entry, invalidate derived files, mark `Finalized`.
fn finalize(
    paths: &MigrationPaths,
    journal: &mut MigrationJournal,
    options: &MigrationOptions,
) -> Result<(), MigrationError> {
    options.check(Failpoint::BeforeFinalize)?;
    options.report(progress_marker(paths, MigrationStep::Finalizing));
    if journal.source_kind == StorageAdapterKind::Jsonl && paths.legacy_jsonl.exists() {
        fs::remove_file(&paths.legacy_jsonl)?;
        sync_dir(&paths.chain_dir);
    }
    update_registry(paths, journal)?;
    if journal.hash_changed {
        remove_derived_files(paths)?;
    }
    journal.error = None;
    journal.set_state(paths, JournalState::Finalized)
}

fn update_registry(
    paths: &MigrationPaths,
    journal: &MigrationJournal,
) -> Result<(), MigrationError> {
    let _guard = crate::CHAIN_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut registry = crate::load_mentisdb_registry(&paths.chain_dir)?;
    let now = Utc::now();
    let created_at = registry
        .chains
        .get(&paths.chain_key)
        .map(|entry| entry.created_at)
        .unwrap_or(now);
    let agents = crate::load_agent_registry(
        &paths.chain_dir,
        &paths.chain_key,
        StorageAdapterKind::Binary,
    )
    .map(|registry| registry.agents.len())
    .unwrap_or(0);
    let entity_types = crate::load_entity_type_registry(
        &paths.chain_dir,
        &paths.chain_key,
        StorageAdapterKind::Binary,
    )
    .map(|registry| registry.entity_types.len())
    .unwrap_or(0);
    registry.chains.insert(
        paths.chain_key.clone(),
        crate::MentisDbRegistration {
            chain_key: paths.chain_key.clone(),
            version: MENTISDB_CURRENT_VERSION,
            storage_adapter: StorageAdapterKind::Binary,
            storage_location: paths.live.display().to_string(),
            thought_count: journal.thoughts_total,
            agent_count: agents,
            entity_type_count: entity_types,
            // Segment information is rewritten with the first normal flush
            // after the chain is opened (it is derived from the chain itself).
            has_private: false,
            public_thought_count: journal.thoughts_total,
            created_at,
            updated_at: now,
        },
    );
    crate::save_mentisdb_registry(&paths.chain_dir, &registry)?;
    Ok(())
}

/// Delete vector sidecars, graphs, WALs and the implicit-edge overlay of a
/// chain whose hashes changed. They are keyed by thought hash, so they would
/// read as stale anyway; removing them lets them rebuild lazily and cleanly.
/// The sidecar *configuration* file is kept.
fn remove_derived_files(paths: &MigrationPaths) -> Result<(), MigrationError> {
    let binary = chain_storage_filename(&paths.chain_key, StorageAdapterKind::Binary);
    let stem = binary
        .strip_suffix(&format!(".{}", StorageAdapterKind::Binary.file_extension()))
        .unwrap_or(&binary);
    let vectors = format!("{stem}.vectors.");
    let overlay = format!("{stem}.auto_edges.bin");
    for entry in fs::read_dir(&paths.chain_dir)?.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let derived =
            (name.starts_with(&vectors) && !name.ends_with(".managed.json")) || name == overlay;
        if derived && entry.path().is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn report_from_journal(paths: &MigrationPaths) -> Result<MigrationReport, MigrationError> {
    let journal =
        MigrationJournal::load(&paths.journal)?.ok_or_else(|| MigrationError::Corrupt {
            index: None,
            message: "journal vanished".to_string(),
        })?;
    Ok(MigrationReport {
        chain_key: journal.chain_key,
        reason: journal.reason,
        from_schema: 0,
        to_schema: MENTISDB_CURRENT_VERSION,
        thoughts: journal.thoughts_total,
        source_bytes: 0,
        staged_bytes: journal.staged_len,
        steps: Vec::new(),
        hash_changed: journal.hash_changed,
        signed: 0,
        torn_tail_bytes: 0,
        elapsed_ms: 0,
        backup_dir: Some(paths.backup_dir.clone()),
        resumed: true,
    })
}

// ---------------------------------------------------------------------------
// recovery, rollback, skip, retry, purge, dry run
// ---------------------------------------------------------------------------

/// Resume or discard an interrupted migration of `chain_key`.
///
/// Called for every journal at daemon start (and by [`run_chain_migration`]).
/// The rules, by journal state when the process died:
///
/// - `Planned`, `BackedUp`: the live chain was never touched. Staging is
///   discarded and [`RecoveryOutcome::Restart`] returned.
/// - `Staged`: if the staged file still matches its recorded digest the commit
///   rename is performed (or recognised as already done) and the run finishes;
///   otherwise staging is discarded and the run restarts.
/// - `Committed`: finalization is repeated.
/// - Terminal states: nothing to do.
///
/// A chain interrupted [`MAX_ATTEMPTS`] times is quarantined instead.
///
/// # Errors
///
/// [`MigrationError::Busy`] when another process is working on the chain, or
/// any error from finishing the interrupted run.
pub fn recover_chain(
    chain_dir: &Path,
    chain_key: &str,
    options: &MigrationOptions,
) -> Result<RecoveryOutcome, MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    match MigrationJournal::load(&paths.journal)? {
        Some(journal) => recover_locked(&paths, journal, options),
        None => Ok(RecoveryOutcome::NothingToDo),
    }
}

fn recover_locked(
    paths: &MigrationPaths,
    mut journal: MigrationJournal,
    options: &MigrationOptions,
) -> Result<RecoveryOutcome, MigrationError> {
    if journal.state.is_terminal() {
        return Ok(RecoveryOutcome::NothingToDo);
    }
    mig_log!(
        warn,
        "chain '{}': resuming interrupted migration (state {:?}, attempt {})",
        paths.chain_key,
        journal.state,
        journal.attempts
    );
    if journal.attempts >= MAX_ATTEMPTS {
        let _ = fs::remove_file(&paths.staged);
        journal.error = Some(format!(
            "migration was interrupted {} times; quarantined so it cannot loop. \
             Fix the cause, then run `mentisdb migrate retry {}`",
            journal.attempts, paths.chain_key
        ));
        journal.set_state(paths, JournalState::Failed)?;
        return Ok(RecoveryOutcome::Quarantined);
    }
    match journal.state {
        JournalState::Planned | JournalState::BackedUp => {
            let _ = fs::remove_file(&paths.staged);
            Ok(RecoveryOutcome::Restart)
        }
        JournalState::Staged => {
            let staged_ok = paths.staged.exists()
                && file_digest(&paths.staged)
                    .map(|(len, digest)| {
                        len == journal.staged_len && digest == journal.staged_digest
                    })
                    .unwrap_or(false);
            let live_is_new = !paths.staged.exists()
                && paths.live.exists()
                && file_digest(&paths.live)
                    .map(|(len, digest)| {
                        len == journal.staged_len && digest == journal.staged_digest
                    })
                    .unwrap_or(false);
            if staged_ok {
                fs::rename(&paths.staged, &paths.live)?;
                sync_dir(&paths.chain_dir);
            } else if !live_is_new {
                let _ = fs::remove_file(&paths.staged);
                return Ok(RecoveryOutcome::Restart);
            }
            journal.set_state(paths, JournalState::Committed)?;
            finalize(paths, &mut journal, options)?;
            Ok(RecoveryOutcome::Completed)
        }
        JournalState::Committed => {
            finalize(paths, &mut journal, options)?;
            Ok(RecoveryOutcome::Completed)
        }
        _ => Ok(RecoveryOutcome::NothingToDo),
    }
}

/// Restore the original chain from the backup taken before migrating.
///
/// Refuses (without `force`) when the migrated chain has changed since the
/// migration, because restoring the backup would discard the thoughts appended
/// since. The registry entry is restored to what it was, derived files are
/// removed so they rebuild, and the journal becomes `RolledBack`, which keeps
/// the chain out of automatic migration until [`retry_chain`].
///
/// # Errors
///
/// [`MigrationError::Refused`] when there is nothing to roll back, the backup
/// is gone, or the chain changed and `force` is false.
pub fn rollback_chain(
    chain_dir: &Path,
    chain_key: &str,
    force: bool,
) -> Result<(), MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    let mut journal = MigrationJournal::load(&paths.journal)?.ok_or_else(|| {
        MigrationError::Refused(format!("chain '{chain_key}' has no migration to roll back"))
    })?;
    if !matches!(
        journal.state,
        JournalState::Committed | JournalState::Finalized
    ) {
        return Err(MigrationError::Refused(format!(
            "chain '{chain_key}' was never changed by a migration (state {:?}); nothing to roll back",
            journal.state
        )));
    }
    let backup = paths.backup_dir.join(&journal.source_file);
    if !backup.exists() {
        return Err(MigrationError::Refused(format!(
            "the backup {} no longer exists",
            backup.display()
        )));
    }
    if !force {
        let unchanged = file_digest(&paths.live)
            .map(|(len, digest)| len == journal.staged_len && digest == journal.staged_digest)
            .unwrap_or(false);
        if !unchanged {
            return Err(MigrationError::Refused(format!(
                "chain '{chain_key}' has changed since it was migrated (thoughts were appended); \
                 rolling back would discard them. Re-run with --force to discard them"
            )));
        }
    }
    let restore_to = paths.source(journal.source_kind).to_path_buf();
    let mut temp = restore_to.as_os_str().to_owned();
    temp.push(".rollback.tmp");
    let temp = PathBuf::from(temp);
    fs::copy(&backup, &temp)?;
    File::open(&temp)?.sync_all()?;
    fs::rename(&temp, &restore_to)?;
    if restore_to != paths.live && paths.live.exists() {
        fs::remove_file(&paths.live)?;
    }
    sync_dir(&paths.chain_dir);
    restore_registration(&paths, &journal)?;
    remove_derived_files(&paths)?;
    journal.error = None;
    journal.set_state(&paths, JournalState::RolledBack)?;
    mig_log!(
        warn,
        "chain '{chain_key}': rolled back to the pre-migration backup"
    );
    Ok(())
}

fn restore_registration(
    paths: &MigrationPaths,
    journal: &MigrationJournal,
) -> Result<(), MigrationError> {
    let _guard = crate::CHAIN_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut registry = crate::load_mentisdb_registry(&paths.chain_dir)?;
    match &journal.original_registration {
        Some(original) => {
            registry
                .chains
                .insert(paths.chain_key.clone(), original.clone());
        }
        None => {
            registry.chains.remove(&paths.chain_key);
        }
    }
    crate::save_mentisdb_registry(&paths.chain_dir, &registry)?;
    Ok(())
}

/// Leave `chain_key` untouched and out of automatic migration.
///
/// # Errors
///
/// [`MigrationError::Refused`] while a migration is mid-flight.
pub fn skip_chain(chain_dir: &Path, chain_key: &str, reason: &str) -> Result<(), MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    let mut journal = match MigrationJournal::load(&paths.journal)? {
        Some(journal) if !journal.state.is_terminal() => {
            return Err(MigrationError::Refused(format!(
                "chain '{chain_key}' is mid-migration (state {:?}); let it finish or recover first",
                journal.state
            )));
        }
        Some(journal) => journal,
        None => {
            let kind = if paths.live.exists() {
                StorageAdapterKind::Binary
            } else {
                StorageAdapterKind::Jsonl
            };
            MigrationJournal::new(&paths, kind, None)
        }
    };
    journal.error = Some(format!("skipped by the operator: {reason}"));
    journal.set_state(&paths, JournalState::Skipped)
}

/// Durably mark `chain_key` as quarantined with `error`, without touching its data.
///
/// Used for problems found before a run starts (an unreadable first record, a
/// newer-than-supported chain), so the quarantine survives a restart and shows
/// in `mentisdb migrate status`.
///
/// # Errors
///
/// [`MigrationError::Refused`] while a migration is mid-flight, or I/O errors.
pub fn quarantine_chain(
    chain_dir: &Path,
    chain_key: &str,
    error: &str,
) -> Result<(), MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    let mut journal = match MigrationJournal::load(&paths.journal)? {
        Some(journal) if !journal.state.is_terminal() => {
            return Err(MigrationError::Refused(format!(
                "chain '{chain_key}' is mid-migration (state {:?})",
                journal.state
            )));
        }
        Some(journal) => journal,
        None => {
            let kind = if paths.live.exists() {
                StorageAdapterKind::Binary
            } else {
                StorageAdapterKind::Jsonl
            };
            MigrationJournal::new(&paths, kind, None)
        }
    };
    journal.error = Some(error.to_owned());
    journal.set_state(&paths, JournalState::Failed)
}

/// Clear a quarantine so the chain is migrated again on the next run.
///
/// Works on `Failed`, `Skipped` and `RolledBack` chains; removes the journal
/// and any staged file.
///
/// # Errors
///
/// [`MigrationError::Refused`] when the chain is mid-migration, or
/// [`MigrationError::Busy`].
pub fn retry_chain(chain_dir: &Path, chain_key: &str) -> Result<(), MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let _lock = ChainLock::acquire(&paths)?;
    match MigrationJournal::load(&paths.journal)? {
        Some(journal) if !journal.state.is_terminal() => Err(MigrationError::Refused(format!(
            "chain '{chain_key}' is mid-migration (state {:?})",
            journal.state
        ))),
        Some(_) => {
            let _ = fs::remove_file(&paths.staged);
            fs::remove_file(&paths.journal)?;
            sync_dir(paths.journal.parent().unwrap_or(&paths.chain_dir));
            Ok(())
        }
        None => Ok(()),
    }
}

/// Delete preserved originals of finalized migrations older than `older_than`
/// (all of them when `None`). Returns the number of backups removed.
///
/// A backup is the only way back, so only `Finalized` ones are eligible; the
/// journal is kept as history.
///
/// # Errors
///
/// Filesystem errors while deleting.
pub fn purge_backups(chain_dir: &Path, older_than: Option<Duration>) -> io::Result<usize> {
    let mut removed = 0;
    for journal in list_journals(chain_dir) {
        if journal.state != JournalState::Finalized {
            continue;
        }
        if let Some(age) = older_than {
            let elapsed = (Utc::now() - journal.updated_at)
                .to_std()
                .unwrap_or_default();
            if elapsed < age {
                continue;
            }
        }
        let paths = MigrationPaths::new(chain_dir, &journal.chain_key);
        if paths.backup_dir.exists() {
            fs::remove_dir_all(&paths.backup_dir)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Rehearse a migration without writing anything.
///
/// Streams the whole chain through the same decode, upgrade and hash-rebuild
/// code as a real run into a sink, so a corrupt chain, a tampered hash or a
/// signature that would break is reported now, with the hash and signature
/// counts a real run would produce. Returns `Ok(None)` for a chain that needs
/// no migration.
///
/// # Errors
///
/// The error a real run would hit.
pub fn dry_run_chain(
    chain_dir: &Path,
    chain_key: &str,
    options: &MigrationOptions,
) -> Result<Option<DryRunReport>, MigrationError> {
    let paths = MigrationPaths::new(chain_dir, chain_key);
    let Some((kind, reason)) = detect_with_kind(&paths)? else {
        return Ok(None);
    };
    let started = Instant::now();
    let mut writer = HashingWriter::new(io::sink());
    let summary = stream_upgrade(&paths, kind, &mut writer, options)?;
    Ok(Some(MigrationReport {
        chain_key: chain_key.to_owned(),
        reason: Some(reason),
        from_schema: summary.from_schema,
        to_schema: MENTISDB_CURRENT_VERSION,
        thoughts: summary.thoughts,
        source_bytes: summary.source_bytes,
        staged_bytes: writer.written,
        steps: summary.steps.iter().map(|s| (*s).to_owned()).collect(),
        hash_changed: summary.hash_changed,
        signed: summary.signed,
        torn_tail_bytes: summary.torn_tail_bytes,
        elapsed_ms: started.elapsed().as_millis() as u64,
        backup_dir: None,
        resumed: false,
    }))
}
