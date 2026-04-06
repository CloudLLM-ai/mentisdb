//! Semantic, hash-chained memory for long-running agents.
//!
//! `mentisdb` provides an append-only, adapter-backed memory log for
//! durable, queryable cognitive state. Thoughts are timestamped, hash-chained,
//! typed, optionally connected to prior thoughts, and exportable as prompts or
//! Markdown memory snapshots. The current default backend is binary, but the
//! chain model is intentionally independent from any single storage format.
#![warn(missing_docs)]

pub mod cli;
#[cfg(feature = "server")]
pub(crate) mod dashboard;
pub mod integrations;
pub mod paths;
pub mod search;
#[cfg(feature = "server")]
pub mod server;
mod skills;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{
    mpsc::{self, SyncSender},
    Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub use skills::{
    export_skill, import_skill, migrate_skill_registry, SkillDocument, SkillEntry, SkillFormat,
    SkillQuery, SkillReadOutput, SkillRegistry, SkillRegistryManifest,
    SkillRegistryMigrationReport, SkillSection, SkillStatus, SkillSummary, SkillUpload, SkillVersion,
    SkillVersionContent, SkillVersionSummary, MENTISDB_SKILL_CURRENT_SCHEMA_VERSION,
    MENTISDB_SKILL_REGISTRY_CURRENT_VERSION, MENTISDB_SKILL_REGISTRY_V1, MENTISDB_SKILL_REGISTRY_V2,
};

/// Persistence interface for MentisDb storage backends.
///
/// Storage adapters are responsible only for durable read and append
/// operations. The in-memory chain model, hashing, querying, and replay logic
/// remain inside [`MentisDb`].
///
/// # Example
///
/// ```
/// use std::path::PathBuf;
/// use mentisdb::{JsonlStorageAdapter, StorageAdapter};
///
/// let adapter = JsonlStorageAdapter::for_chain_key(PathBuf::from("/tmp/tc_store"), "demo");
/// let location = adapter.storage_location();
///
/// assert!(location.ends_with(".jsonl"));
/// ```
pub trait StorageAdapter: Send + Sync {
    /// Load all persisted thoughts in order.
    fn load_thoughts(&self) -> io::Result<Vec<Thought>>;

    /// Persist a newly appended thought.
    fn append_thought(&self, thought: &Thought) -> io::Result<()>;

    /// Flush any pending buffered writes to the backing store.
    ///
    /// Adapters that do not buffer writes may leave the default no-op
    /// implementation.
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }

    /// Reconfigure whether appends should be flushed immediately.
    ///
    /// Adapters that do not support write-mode reconfiguration may leave the
    /// default no-op implementation.
    fn set_auto_flush(&self, _auto_flush: bool) -> io::Result<()> {
        Ok(())
    }

    /// Return a human-readable storage location or descriptor.
    fn storage_location(&self) -> String;

    /// Return the durable storage adapter kind.
    fn storage_kind(&self) -> StorageAdapterKind;

    /// Return the concrete backing path when the adapter is file-based.
    fn storage_path(&self) -> Option<&Path>;
}

/// Legacy MentisDb storage schema version.
pub const MENTISDB_SCHEMA_V0: u32 = 0;
/// First registry-backed MentisDb storage schema version.
pub const MENTISDB_SCHEMA_V1: u32 = 1;
/// Schema version 2: adds [`ThoughtType::Reframe`], [`ThoughtRelationKind::Supersedes`],
/// and optional cross-chain [`ThoughtRelation::chain_key`].
pub const MENTISDB_SCHEMA_V2: u32 = 2;
/// Alias for the latest supported MentisDb storage schema version.
pub const MENTISDB_CURRENT_VERSION: u32 = MENTISDB_SCHEMA_V2;
const MENTISDB_REGISTRY_FILENAME: &str = "mentisdb-registry.json";
const LEGACY_THOUGHTCHAIN_REGISTRY_FILENAME: &str = "thoughtchain-registry.json";

fn current_schema_version() -> u32 {
    MENTISDB_CURRENT_VERSION
}

/// Supported durable storage formats for MentisDb.
///
/// This enum lets applications select a backend without hard-coding a concrete
/// adapter type. `Jsonl` remains the most human-inspectable option, while
/// `Binary` stores length-prefixed serialized thoughts for more compact and
/// efficient loading.
///
/// # Example
///
/// ```
/// use std::str::FromStr;
/// use mentisdb::StorageAdapterKind;
///
/// let kind = StorageAdapterKind::from_str("binary").unwrap();
/// assert_eq!(kind.as_str(), "binary");
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum StorageAdapterKind {
    /// Length-prefixed binary serialization of `Thought` records.
    #[default]
    Binary,
    /// Newline-delimited JSON storage.
    Jsonl,
}

impl StorageAdapterKind {
    /// Return the stable lowercase name of this adapter kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Binary => "binary",
        }
    }

    /// Return the file extension used by this adapter kind.
    pub fn file_extension(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Binary => "tcbin",
        }
    }

    /// Create a boxed storage adapter for a durable chain key.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{StorageAdapter, StorageAdapterKind};
    ///
    /// let adapter = StorageAdapterKind::Binary
    ///     .for_chain_key(PathBuf::from("/tmp/tc_kind"), "demo");
    /// assert!(adapter.storage_location().ends_with(".tcbin"));
    /// ```
    pub fn for_chain_key<P: AsRef<Path>>(
        self,
        chain_dir: P,
        chain_key: &str,
    ) -> Box<dyn StorageAdapter> {
        match self {
            Self::Jsonl => Box::new(JsonlStorageAdapter::for_chain_key(chain_dir, chain_key)),
            Self::Binary => Box::new(BinaryStorageAdapter::for_chain_key(chain_dir, chain_key)),
        }
    }
}

impl fmt::Display for StorageAdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StorageAdapterKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "jsonl" => Ok(Self::Jsonl),
            "binary" => Ok(Self::Binary),
            other => Err(format!(
                "Unsupported MentisDb storage adapter '{other}'. Expected 'jsonl' or 'binary'"
            )),
        }
    }
}

/// Append-only JSONL storage adapter for MentisDb.
///
/// This is the default storage backend used by [`MentisDb::open`] and
/// [`MentisDb::open_with_key`].
///
/// # Example
///
/// ```rust,no_run
/// use std::path::PathBuf;
/// use mentisdb::{JsonlStorageAdapter, StorageAdapter};
///
/// let adapter = JsonlStorageAdapter::for_chain_key(PathBuf::from("/tmp/tc_jsonl"), "agent-memory");
/// assert!(adapter.storage_location().ends_with(".jsonl"));
/// ```
#[derive(Debug, Clone)]
pub struct JsonlStorageAdapter {
    file_path: PathBuf,
}

impl JsonlStorageAdapter {
    /// Create a JSONL adapter for an explicit file path.
    pub fn new(file_path: PathBuf) -> Self {
        Self { file_path }
    }

    /// Create a JSONL adapter using the stable MentisDb filename for a chain key.
    pub fn for_chain_key<P: AsRef<Path>>(chain_dir: P, chain_key: &str) -> Self {
        let file_path = chain_dir
            .as_ref()
            .join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl));
        Self::new(file_path)
    }

    /// Return the underlying JSONL path.
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }
}

impl StorageAdapter for JsonlStorageAdapter {
    fn load_thoughts(&self) -> io::Result<Vec<Thought>> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.file_path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let thought: Thought = serde_json::from_str(&line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Failed to parse thought: {e}"),
                )
            })?;
            entries.push(thought);
        }
        Ok(entries)
    }

    fn append_thought(&self, thought: &Thought) -> io::Result<()> {
        persist_jsonl_thought(&self.file_path, thought)
    }

    fn storage_location(&self) -> String {
        self.file_path.display().to_string()
    }

    fn storage_kind(&self) -> StorageAdapterKind {
        StorageAdapterKind::Jsonl
    }

    fn storage_path(&self) -> Option<&Path> {
        Some(self.file_path.as_path())
    }
}

/// Append-only binary storage adapter for MentisDb.
///
/// Each record is stored as a length-prefixed bincode-serialized [`Thought`],
/// keeping append operations simple while avoiding JSON parse overhead on
/// reload.
///
/// ## Write buffering
///
/// When `auto_flush = true` (the default), appends are queued to a dedicated
/// background writer and the caller blocks until the writer flushes them to the
/// OS. Concurrent requests can share a short group-commit window, preserving
/// durable-ack semantics while reducing contention on highly concurrent write
/// workloads.
///
/// When `auto_flush = false`, appends are handed to a bounded background-writer
/// queue. The worker batches records and flushes them to disk every
/// [`FLUSH_THRESHOLD`] entries (or when the adapter is explicitly flushed or
/// dropped). This mode dramatically increases batch-append throughput at the
/// cost of potentially losing the current in-memory batch plus any queued
/// acknowleded appends on a hard crash.
///
/// ## Clone behaviour
///
/// Cloning creates a *new* adapter for the same file path with a fresh, empty
/// write state (no background writer). The clone is suitable for use by a
/// second independent reader/writer.
///
/// # Example
///
/// ```rust,no_run
/// use std::path::PathBuf;
/// use mentisdb::{BinaryStorageAdapter, StorageAdapter};
///
/// let adapter = BinaryStorageAdapter::for_chain_key(PathBuf::from("/tmp/tc_bin"), "agent-memory");
/// assert!(adapter.storage_location().ends_with(".tcbin"));
/// ```
pub struct BinaryStorageAdapter {
    file_path: PathBuf,
    /// Interior-mutable write state.  The `Mutex` allows `&self` calls in the
    /// [`StorageAdapter`] trait to mutate the file handle and background writer.
    state: Mutex<WriterState>,
    background_error: Arc<Mutex<Option<BackgroundWriteError>>>,
}

impl std::fmt::Debug for BinaryStorageAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auto_flush = self
            .state
            .lock()
            .expect("BinaryStorageAdapter state mutex poisoned")
            .auto_flush;
        f.debug_struct("BinaryStorageAdapter")
            .field("file_path", &self.file_path)
            .field("auto_flush", &auto_flush)
            .finish_non_exhaustive()
    }
}

/// Manual `Clone`: produces a fresh adapter for the same path.
///
/// The clone does **not** share the open file handle or any buffered bytes —
/// it starts with a clean writer state, identical to a newly constructed
/// adapter.
impl Clone for BinaryStorageAdapter {
    fn clone(&self) -> Self {
        Self::with_auto_flush(self.file_path.clone(), self.is_auto_flush())
    }
}

/// Number of deferred appends before the write buffer is flushed to disk.
///
/// Only relevant when `BinaryStorageAdapter::auto_flush = false`.
pub const FLUSH_THRESHOLD: usize = 16;

/// Number of queued append requests allowed before writers backpressure callers.
const BACKGROUND_WRITE_QUEUE_CAPACITY: usize = FLUSH_THRESHOLD * 4;

/// Maximum time a durable append waits for more work before forcing a flush.
const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(2);

/// Number of append-driven chain-registration count updates to batch before
/// rewriting the global registry file.
const CHAIN_REGISTRATION_FLUSH_THRESHOLD: usize = FLUSH_THRESHOLD;

/// Number of append-driven agent-registry sidecar updates to batch before
/// rewriting the per-chain registry JSON when buffered writes are enabled.
const AGENT_REGISTRY_FLUSH_THRESHOLD: usize = FLUSH_THRESHOLD;

#[derive(Debug, Clone)]
struct BackgroundWriteError {
    kind: io::ErrorKind,
    message: String,
}

impl BackgroundWriteError {
    fn from_io_error(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
        }
    }

    fn to_io_error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

enum WriteCommand {
    Append {
        payload: Vec<u8>,
        ack_tx: Option<mpsc::Sender<io::Result<()>>>,
    },
    Flush(mpsc::Sender<io::Result<()>>),
    Shutdown(mpsc::Sender<io::Result<()>>),
}

struct BackgroundWriter {
    tx: SyncSender<WriteCommand>,
    join: JoinHandle<()>,
}

/// Mutable write state held inside [`BinaryStorageAdapter`].
struct WriterState {
    auto_flush: bool,
    /// Background writer used for both strict and buffered modes.
    background_writer: Option<BackgroundWriter>,
}

impl WriterState {
    fn new(auto_flush: bool) -> Self {
        Self {
            auto_flush,
            background_writer: None,
        }
    }

    fn ensure_background_writer(
        &mut self,
        file_path: &Path,
        error_state: &Arc<Mutex<Option<BackgroundWriteError>>>,
    ) -> io::Result<&BackgroundWriter> {
        if self.background_writer.is_none() {
            clear_background_error(error_state);
            self.background_writer = Some(BackgroundWriter::spawn(
                file_path.to_path_buf(),
                error_state.clone(),
            )?);
        }
        Ok(self.background_writer.as_ref().unwrap())
    }
}

fn open_append_writer(file_path: &Path) -> io::Result<BufWriter<File>> {
    if let Some(parent) = file_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    Ok(BufWriter::new(file))
}

fn flush_background_buffer(
    file: &mut Option<BufWriter<File>>,
    file_path: &Path,
    write_buffer: &mut Vec<u8>,
    dirty_count: &mut usize,
) -> io::Result<()> {
    if write_buffer.is_empty() {
        return Ok(());
    }
    if file.is_none() {
        *file = Some(open_append_writer(file_path)?);
    }
    let writer = file.as_mut().unwrap();
    writer.write_all(write_buffer)?;
    writer.flush()?;
    write_buffer.clear();
    *dirty_count = 0;
    Ok(())
}

fn set_background_error(error_state: &Arc<Mutex<Option<BackgroundWriteError>>>, error: &io::Error) {
    if let Ok(mut slot) = error_state.lock() {
        if slot.is_none() {
            *slot = Some(BackgroundWriteError::from_io_error(error));
        }
    }
}

fn clear_background_error(error_state: &Arc<Mutex<Option<BackgroundWriteError>>>) {
    if let Ok(mut slot) = error_state.lock() {
        *slot = None;
    }
}

fn current_background_error(
    error_state: &Arc<Mutex<Option<BackgroundWriteError>>>,
) -> Option<io::Error> {
    error_state
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(BackgroundWriteError::to_io_error))
}

fn clone_io_error(error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), error.to_string())
}

fn clone_io_result(result: &io::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(clone_io_error(error)),
    }
}

impl BackgroundWriter {
    fn spawn(
        file_path: PathBuf,
        error_state: Arc<Mutex<Option<BackgroundWriteError>>>,
    ) -> io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(BACKGROUND_WRITE_QUEUE_CAPACITY);
        let join = thread::Builder::new()
            .name("mentisdb-binary-writer".to_string())
            .spawn(move || {
                enum PostFlushAction {
                    Flush(mpsc::Sender<io::Result<()>>),
                    Shutdown(Option<mpsc::Sender<io::Result<()>>>),
                }

                let mut file = None;
                let mut write_buffer = Vec::new();
                let mut dirty_count = 0usize;
                let mut durable_acks: Vec<mpsc::Sender<io::Result<()>>> = Vec::new();
                while let Ok(command) = rx.recv() {
                    let result = match command {
                        WriteCommand::Append { payload, ack_tx } => {
                            write_buffer.extend_from_slice(&payload);
                            dirty_count += 1;
                            if let Some(ack_tx) = ack_tx {
                                durable_acks.push(ack_tx);
                                let deadline = Instant::now() + GROUP_COMMIT_WINDOW;
                                let mut follow_up: Option<PostFlushAction> = None;
                                loop {
                                    let remaining =
                                        deadline.saturating_duration_since(Instant::now());
                                    if remaining.is_zero() {
                                        break;
                                    }
                                    match rx.recv_timeout(remaining) {
                                        Ok(WriteCommand::Append { payload, ack_tx }) => {
                                            write_buffer.extend_from_slice(&payload);
                                            dirty_count += 1;
                                            if let Some(ack_tx) = ack_tx {
                                                durable_acks.push(ack_tx);
                                            }
                                        }
                                        Ok(WriteCommand::Flush(ack_tx)) => {
                                            follow_up = Some(PostFlushAction::Flush(ack_tx));
                                            break;
                                        }
                                        Ok(WriteCommand::Shutdown(ack_tx)) => {
                                            follow_up =
                                                Some(PostFlushAction::Shutdown(Some(ack_tx)));
                                            break;
                                        }
                                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                                            follow_up = Some(PostFlushAction::Shutdown(None));
                                            break;
                                        }
                                    }
                                }
                                let result = flush_background_buffer(
                                    &mut file,
                                    &file_path,
                                    &mut write_buffer,
                                    &mut dirty_count,
                                );
                                if let Err(error) = &result {
                                    set_background_error(&error_state, error);
                                }
                                for ack in durable_acks.drain(..) {
                                    let _ = ack.send(clone_io_result(&result));
                                }
                                match follow_up {
                                    Some(PostFlushAction::Flush(ack_tx)) => {
                                        let _ = ack_tx.send(clone_io_result(&result));
                                        result
                                    }
                                    Some(PostFlushAction::Shutdown(ack_tx)) => {
                                        if let Some(ack_tx) = ack_tx {
                                            let _ = ack_tx.send(clone_io_result(&result));
                                        }
                                        break;
                                    }
                                    None => result,
                                }
                            } else if dirty_count >= FLUSH_THRESHOLD {
                                flush_background_buffer(
                                    &mut file,
                                    &file_path,
                                    &mut write_buffer,
                                    &mut dirty_count,
                                )
                            } else {
                                Ok(())
                            }
                        }
                        WriteCommand::Flush(ack_tx) => {
                            let result = flush_background_buffer(
                                &mut file,
                                &file_path,
                                &mut write_buffer,
                                &mut dirty_count,
                            );
                            let response = match &result {
                                Ok(()) => Ok(()),
                                Err(error) => {
                                    set_background_error(&error_state, error);
                                    Err(io::Error::new(error.kind(), error.to_string()))
                                }
                            };
                            let _ = ack_tx.send(response);
                            result
                        }
                        WriteCommand::Shutdown(ack_tx) => {
                            let result = flush_background_buffer(
                                &mut file,
                                &file_path,
                                &mut write_buffer,
                                &mut dirty_count,
                            );
                            let response = match &result {
                                Ok(()) => Ok(()),
                                Err(error) => {
                                    set_background_error(&error_state, error);
                                    Err(io::Error::new(error.kind(), error.to_string()))
                                }
                            };
                            let _ = ack_tx.send(response);
                            break;
                        }
                    };
                    if let Err(error) = result {
                        set_background_error(&error_state, &error);
                        break;
                    }
                }
                if let Err(error) = flush_background_buffer(
                    &mut file,
                    &file_path,
                    &mut write_buffer,
                    &mut dirty_count,
                ) {
                    set_background_error(&error_state, &error);
                }
            })
            .map_err(|error| io::Error::other(format!("Failed to spawn binary writer: {error}")))?;
        Ok(Self { tx, join })
    }

    fn append(
        &self,
        payload: Vec<u8>,
        durable: bool,
        error_state: &Arc<Mutex<Option<BackgroundWriteError>>>,
    ) -> io::Result<()> {
        if let Some(error) = current_background_error(error_state) {
            return Err(error);
        }
        if durable {
            let (ack_tx, ack_rx) = mpsc::channel();
            self.tx
                .send(WriteCommand::Append {
                    payload,
                    ack_tx: Some(ack_tx),
                })
                .map_err(|_| {
                    current_background_error(error_state).unwrap_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Binary background writer thread stopped",
                        )
                    })
                })?;
            ack_rx.recv().unwrap_or_else(|_| {
                Err(current_background_error(error_state).unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Binary background writer acknowledgement channel closed",
                    )
                }))
            })
        } else {
            self.tx
                .send(WriteCommand::Append {
                    payload,
                    ack_tx: None,
                })
                .map_err(|_| {
                    current_background_error(error_state).unwrap_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "Binary background writer thread stopped",
                        )
                    })
                })
        }
    }

    fn flush(&self, error_state: &Arc<Mutex<Option<BackgroundWriteError>>>) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        self.tx.send(WriteCommand::Flush(ack_tx)).map_err(|_| {
            current_background_error(error_state).unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Binary background writer thread stopped",
                )
            })
        })?;
        ack_rx.recv().unwrap_or_else(|_| {
            Err(current_background_error(error_state).unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Binary background writer acknowledgement channel closed",
                )
            }))
        })
    }

    fn shutdown(self, error_state: &Arc<Mutex<Option<BackgroundWriteError>>>) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        let send_result = self.tx.send(WriteCommand::Shutdown(ack_tx));
        let ack_result = match send_result {
            Ok(()) => ack_rx.recv().unwrap_or_else(|_| {
                Err(current_background_error(error_state).unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Binary background writer acknowledgement channel closed",
                    )
                }))
            }),
            Err(_) => Err(current_background_error(error_state).unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Binary background writer thread stopped",
                )
            })),
        };
        let join_result = self.join.join();
        if join_result.is_err() {
            return Err(io::Error::other("Binary background writer thread panicked"));
        }
        ack_result
    }
}

impl BinaryStorageAdapter {
    /// Create a binary adapter for an explicit file path with `auto_flush = true`.
    pub fn new(file_path: PathBuf) -> Self {
        Self::with_auto_flush(file_path, true)
    }

    /// Create a binary adapter with an explicit `auto_flush` setting.
    ///
    /// Pass `auto_flush = true` for durable group-commit acknowledgements or
    /// `auto_flush = false` for fire-and-forget batching. Call
    /// [`flush`][Self::flush] or rely on the `Drop` impl to persist the final
    /// buffered batch before shutdown.
    pub fn with_auto_flush(file_path: PathBuf, auto_flush: bool) -> Self {
        Self {
            file_path,
            state: Mutex::new(WriterState::new(auto_flush)),
            background_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a binary adapter using the stable MentisDb filename for a chain key.
    pub fn for_chain_key<P: AsRef<Path>>(chain_dir: P, chain_key: &str) -> Self {
        let file_path = chain_dir.as_ref().join(chain_storage_filename(
            chain_key,
            StorageAdapterKind::Binary,
        ));
        Self::new(file_path)
    }

    /// Return the underlying binary path.
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Return whether this adapter requires a durable flush acknowledgement for
    /// every append.
    pub fn is_auto_flush(&self) -> bool {
        self.state
            .lock()
            .expect("BinaryStorageAdapter state mutex poisoned")
            .auto_flush
    }

    /// Flush any queued or buffered bytes to disk immediately.
    ///
    /// In both modes this blocks until the background writer drains its queue
    /// and flushes all pending records to the OS. In `auto_flush = true`, each
    /// append already waits for a durable flush, so this is usually only needed
    /// as an explicit barrier before reads or shutdown.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the underlying write or flush fails.
    pub fn flush(&self) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .expect("BinaryStorageAdapter state mutex poisoned");
        if let Some(worker) = state.background_writer.as_ref() {
            return worker.flush(&self.background_error);
        }
        if let Some(error) = current_background_error(&self.background_error) {
            return Err(error);
        }
        Ok(())
    }
}

impl Drop for BinaryStorageAdapter {
    /// Flush any remaining buffered bytes to disk on drop.
    ///
    /// Errors are silently ignored here (we cannot propagate them from `Drop`).
    /// Callers that require guaranteed durability should call [`flush`][BinaryStorageAdapter::flush]
    /// explicitly before dropping.
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(worker) = state.background_writer.take() {
                let _ = worker.shutdown(&self.background_error);
            }
        }
    }
}

impl StorageAdapter for BinaryStorageAdapter {
    fn load_thoughts(&self) -> io::Result<Vec<Thought>> {
        // Flush any buffered writes so the file reflects the full chain state
        // before we re-read it.
        BinaryStorageAdapter::flush(self)?;
        load_binary_thoughts(&self.file_path)
    }

    fn append_thought(&self, thought: &Thought) -> io::Result<()> {
        // Serialize the thought to its length-prefixed wire format.
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard())
            .map_err(|e| io::Error::other(format!("Failed to serialize thought: {e}")))?;
        let length_bytes = (payload.len() as u64).to_le_bytes();

        let mut state = self
            .state
            .lock()
            .expect("BinaryStorageAdapter state mutex poisoned");
        let durable = state.auto_flush;
        let worker = state.ensure_background_writer(&self.file_path, &self.background_error)?;
        let mut record = Vec::with_capacity(length_bytes.len() + payload.len());
        record.extend_from_slice(&length_bytes);
        record.extend_from_slice(&payload);
        worker.append(record, durable, &self.background_error)
    }

    fn flush(&self) -> io::Result<()> {
        BinaryStorageAdapter::flush(self)
    }

    fn set_auto_flush(&self, auto_flush: bool) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .expect("BinaryStorageAdapter state mutex poisoned");
        if state.auto_flush == auto_flush {
            if auto_flush {
                state.ensure_background_writer(&self.file_path, &self.background_error)?;
            }
            return Ok(());
        }

        if auto_flush {
            if let Some(worker) = state.background_writer.as_ref() {
                worker.flush(&self.background_error)?;
            }
            state.auto_flush = true;
            return Ok(());
        }

        state.ensure_background_writer(&self.file_path, &self.background_error)?;
        state.auto_flush = false;
        Ok(())
    }

    fn storage_location(&self) -> String {
        self.file_path.display().to_string()
    }

    fn storage_kind(&self) -> StorageAdapterKind {
        StorageAdapterKind::Binary
    }

    fn storage_path(&self) -> Option<&Path> {
        Some(self.file_path.as_path())
    }
}

/// Supported public-key algorithms for agent identity records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PublicKeyAlgorithm {
    /// Ed25519 signing keys.
    Ed25519,
}

impl PublicKeyAlgorithm {
    /// Return the stable lowercase name of this key algorithm.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ed25519 => "ed25519",
        }
    }
}

impl fmt::Display for PublicKeyAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PublicKeyAlgorithm {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ed25519" => Ok(Self::Ed25519),
            other => Err(format!(
                "Unsupported MentisDb public-key algorithm '{other}'. Expected 'ed25519'"
            )),
        }
    }
}

/// Public verification key associated with an agent identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentPublicKey {
    /// Stable identifier for the key.
    pub key_id: String,
    /// Cryptographic algorithm used by the key.
    pub algorithm: PublicKeyAlgorithm,
    /// Raw public-key bytes.
    pub public_key_bytes: Vec<u8>,
    /// UTC timestamp when the key was registered.
    pub added_at: DateTime<Utc>,
    /// UTC timestamp when the key was revoked, if any.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Current status of an agent record in the registry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// The agent is active.
    Active,
    /// The agent has been revoked or retired.
    Revoked,
}

impl AgentStatus {
    /// Return the stable lowercase name of this agent status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "revoked" | "disabled" => Ok(Self::Revoked),
            other => Err(format!(
                "Unsupported MentisDb agent status '{other}'. Expected 'active' or 'revoked'"
            )),
        }
    }
}

/// Registry entry describing one durable agent identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRecord {
    /// Stable producer identifier used in thoughts.
    pub agent_id: String,
    /// Friendly display label for the agent.
    pub display_name: String,
    /// Optional owner, tenant, or grouping label.
    pub owner: Option<String>,
    /// Optional summary of what the agent does.
    pub description: Option<String>,
    /// Historical display-name aliases.
    pub aliases: Vec<String>,
    /// Public verification keys associated with the agent.
    pub public_keys: Vec<AgentPublicKey>,
    /// Lifecycle status of the agent identity.
    pub status: AgentStatus,
    /// First thought index observed for this agent in the chain.
    pub first_seen_index: Option<u64>,
    /// Most recent thought index observed for this agent in the chain.
    pub last_seen_index: Option<u64>,
    /// First observed timestamp for this agent in the chain.
    pub first_seen_at: Option<DateTime<Utc>>,
    /// Most recent observed timestamp for this agent in the chain.
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Number of thoughts attributed to this agent in the chain.
    pub thought_count: u64,
}

impl AgentRecord {
    fn stub(agent_id: &str) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            display_name: agent_id.to_string(),
            owner: None,
            description: None,
            aliases: Vec::new(),
            public_keys: Vec::new(),
            status: AgentStatus::Active,
            first_seen_index: None,
            last_seen_index: None,
            first_seen_at: None,
            last_seen_at: None,
            thought_count: 0,
        }
    }

    fn new(
        agent_id: &str,
        display_name: &str,
        owner: Option<&str>,
        index: u64,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            agent_id: agent_id.to_string(),
            display_name: if display_name.trim().is_empty() {
                agent_id.to_string()
            } else {
                display_name.trim().to_string()
            },
            owner: owner
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            description: None,
            aliases: Vec::new(),
            public_keys: Vec::new(),
            status: AgentStatus::Active,
            first_seen_index: Some(index),
            last_seen_index: Some(index),
            first_seen_at: Some(timestamp),
            last_seen_at: Some(timestamp),
            thought_count: 1,
        }
    }

    fn observe(
        &mut self,
        display_name: Option<&str>,
        owner: Option<&str>,
        index: u64,
        timestamp: DateTime<Utc>,
    ) {
        if let Some(display_name) = display_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if !equals_case_insensitive(display_name, &self.display_name)
                && !self
                    .aliases
                    .iter()
                    .any(|alias| equals_case_insensitive(alias, display_name))
            {
                self.aliases.push(self.display_name.clone());
                self.display_name = display_name.to_string();
            }
        }

        if self.owner.is_none() {
            self.owner = owner
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
        }

        self.last_seen_index = Some(index);
        self.last_seen_at = Some(timestamp);
        self.thought_count += 1;
    }

    fn set_display_name(&mut self, display_name: &str) {
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name == self.display_name {
            return;
        }
        if !equals_case_insensitive(display_name, &self.display_name)
            && !self
                .aliases
                .iter()
                .any(|alias| equals_case_insensitive(alias, display_name))
        {
            self.aliases.push(self.display_name.clone());
        }
        self.display_name = display_name.to_string();
    }

    fn set_owner(&mut self, owner: Option<&str>) {
        self.owner = owner
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }

    fn set_description(&mut self, description: Option<&str>) {
        self.description = description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }

    fn add_alias(&mut self, alias: &str) {
        let alias = alias.trim();
        if alias.is_empty()
            || equals_case_insensitive(alias, &self.display_name)
            || self
                .aliases
                .iter()
                .any(|existing| equals_case_insensitive(existing, alias))
        {
            return;
        }
        self.aliases.push(alias.to_string());
    }

    fn add_public_key(&mut self, key: AgentPublicKey) {
        if let Some(existing) = self
            .public_keys
            .iter_mut()
            .find(|existing| existing.key_id == key.key_id)
        {
            *existing = key;
        } else {
            self.public_keys.push(key);
        }
    }

    fn revoke_key(&mut self, key_id: &str, revoked_at: DateTime<Utc>) -> bool {
        if let Some(existing) = self.public_keys.iter_mut().find(|key| key.key_id == key_id) {
            existing.revoked_at = Some(revoked_at);
            true
        } else {
            false
        }
    }
}

/// Per-chain registry of the agents that have written thoughts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentRegistry {
    /// Registry entries keyed by stable `agent_id`.
    pub agents: BTreeMap<String, AgentRecord>,
}

impl AgentRegistry {
    fn observe(
        &mut self,
        agent_id: &str,
        display_name: Option<&str>,
        owner: Option<&str>,
        index: u64,
        timestamp: DateTime<Utc>,
    ) {
        match self.agents.get_mut(agent_id) {
            Some(record) => record.observe(display_name, owner, index, timestamp),
            None => {
                let display_name = display_name.unwrap_or(agent_id);
                let record = AgentRecord::new(agent_id, display_name, owner, index, timestamp);
                self.agents.insert(agent_id.to_string(), record);
            }
        }
    }
}

/// Metadata describing one registered thought chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentisDbRegistration {
    /// Stable chain identifier.
    pub chain_key: String,
    /// Storage schema version for the active chain file.
    pub version: u32,
    /// Storage adapter used by the active chain file.
    pub storage_adapter: StorageAdapterKind,
    /// Human-readable location of the active chain file.
    pub storage_location: String,
    /// Number of persisted thoughts in the chain.
    pub thought_count: u64,
    /// Number of agents in the per-chain registry.
    pub agent_count: usize,
    /// UTC timestamp when the registration was created.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp when the registration was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Registry of all known thought chains in one storage directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MentisDbRegistry {
    /// Version of the registry file itself.
    pub version: u32,
    /// Registered chains keyed by stable `chain_key`.
    pub chains: BTreeMap<String, MentisDbRegistration>,
}

/// Summary of a successful chain migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MentisDbMigrationReport {
    /// Stable chain identifier.
    pub chain_key: String,
    /// Previous storage schema version.
    pub from_version: u32,
    /// New storage schema version.
    pub to_version: u32,
    /// Storage adapter used by the source chain file.
    pub source_storage_adapter: StorageAdapterKind,
    /// Storage adapter used by the migrated chain.
    pub storage_adapter: StorageAdapterKind,
    /// Number of migrated thoughts.
    pub thought_count: u64,
    /// Path where the legacy chain file was archived.
    pub archived_legacy_path: Option<PathBuf>,
}

/// Progress notifications emitted during migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentisDbMigrationEvent {
    /// A migration run is starting for a chain.
    Started {
        /// Stable chain identifier.
        chain_key: String,
        /// Previous storage schema version.
        from_version: u32,
        /// Target storage schema version.
        to_version: u32,
        /// One-based chain counter within this migration run.
        current: usize,
        /// Total number of chains in this migration run.
        total: usize,
    },
    /// A chain finished migrating successfully.
    Completed {
        /// Stable chain identifier.
        chain_key: String,
        /// Previous storage schema version.
        from_version: u32,
        /// Target storage schema version.
        to_version: u32,
        /// One-based chain counter within this migration run.
        current: usize,
        /// Total number of chains in this migration run.
        total: usize,
    },
    /// A current-version chain is being reconciled to the target storage adapter
    /// or repaired after an integrity/storage mismatch.
    StartedReconciliation {
        /// Stable chain identifier.
        chain_key: String,
        /// Storage adapter used by the source chain file.
        from_storage_adapter: StorageAdapterKind,
        /// Storage adapter expected after reconciliation.
        to_storage_adapter: StorageAdapterKind,
        /// One-based chain counter within this reconciliation run.
        current: usize,
        /// Total number of chains in this reconciliation run.
        total: usize,
    },
    /// A current-version chain finished reconciling successfully.
    CompletedReconciliation {
        /// Stable chain identifier.
        chain_key: String,
        /// Storage adapter used by the source chain file.
        from_storage_adapter: StorageAdapterKind,
        /// Storage adapter expected after reconciliation.
        to_storage_adapter: StorageAdapterKind,
        /// One-based chain counter within this reconciliation run.
        current: usize,
        /// Total number of chains in this reconciliation run.
        total: usize,
    },
}

/// Semantic category describing what changed in the agent's internal model.
///
/// `ThoughtType` is intentionally semantic rather than operational. For example,
/// `Summary` describes the meaning of the thought, while
/// [`ThoughtRole::Compression`] captures why it was emitted.
///
/// # Example
///
/// ```
/// use mentisdb::ThoughtType;
///
/// let thought_type = ThoughtType::Constraint;
/// let json = serde_json::to_string(&thought_type).unwrap();
///
/// assert_eq!(json, "\"Constraint\"");
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ThoughtType {
    /// A user's stated preference changed or became explicit.
    PreferenceUpdate,
    /// A durable characteristic of the user was learned.
    UserTrait,
    /// The agent's model of its relationship with the user changed.
    RelationshipUpdate,
    /// A concrete observation was recorded.
    Finding,
    /// A higher-level synthesis or realization was recorded.
    Insight,
    /// A factual piece of information was learned.
    FactLearned,
    /// A recurring pattern was detected across events or interactions.
    PatternDetected,
    /// A tentative explanation or prediction was formed.
    Hypothesis,
    /// The agent recorded an error in its prior reasoning or action.
    Mistake,
    /// The agent recorded the corrected version of a prior mistake.
    Correction,
    /// A durable lesson or operating heuristic was distilled from prior work.
    LessonLearned,
    /// A previously trusted assumption was invalidated.
    AssumptionInvalidated,
    /// A requirement or hard limit was identified.
    Constraint,
    /// A plan for future work was created or updated.
    Plan,
    /// A smaller unit of work was carved out from a broader plan.
    Subgoal,
    /// A concrete choice was made.
    Decision,
    /// The agent changed its overall approach.
    StrategyShift,
    /// An open-ended curiosity or line of exploration was recorded.
    Wonder,
    /// An unresolved question was recorded.
    Question,
    /// A possible future direction or design concept was proposed.
    Idea,
    /// An experiment or trial was proposed or executed.
    Experiment,
    /// A meaningful action was performed.
    ActionTaken,
    /// A task or milestone was completed.
    TaskComplete,
    /// A checkpoint suitable for resumption was recorded.
    Checkpoint,
    /// A broader snapshot of current state was recorded.
    StateSnapshot,
    /// Work or context was explicitly handed to another actor.
    Handoff,
    /// A summary view of prior thoughts was recorded.
    Summary,
    /// An unexpected outcome or mismatch was observed.
    Surprise,
    /// The agent recontextualised a prior thought, negative pattern, or anchoring
    /// error without denying or deleting it.  Use `Reframe` when the original
    /// thought was accurate but its framing was unhelpful, and a durable shift in
    /// interpretation should be recorded alongside the original.
    ///
    /// # NOTE FOR MAINTAINERS
    /// New variants must ALWAYS be appended at the END of this enum.
    /// Bincode encodes variants by their integer index; inserting mid-enum
    /// shifts all subsequent indices and silently corrupts persisted data.
    Reframe,
}

/// Operational role of a thought inside the system.
///
/// Roles answer how a thought is being used by the system, which lets callers
/// distinguish semantic meaning from lifecycle mechanics.
///
/// # Example
///
/// ```
/// use mentisdb::ThoughtRole;
///
/// assert_eq!(ThoughtRole::default(), ThoughtRole::Memory);
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum ThoughtRole {
    /// Durable long-term memory.
    #[default]
    Memory,
    /// Shorter-lived or more speculative working memory.
    WorkingMemory,
    /// A synthesized summary role.
    Summary,
    /// A role emitted during context compression.
    Compression,
    /// A role intended primarily for resumption checkpoints.
    Checkpoint,
    /// A role intended for handoff to another actor or process.
    Handoff,
    /// A role intended mainly for traceability or audit logs.
    Audit,
    /// A role emitted during deliberate post-incident or post-struggle reflection.
    Retrospective,
}

/// Why a thought points to another thought.
///
/// # Example
///
/// ```
/// use mentisdb::ThoughtRelationKind;
///
/// assert_eq!(ThoughtRelationKind::Corrects as u8, ThoughtRelationKind::Corrects as u8);
/// ```
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ThoughtRelationKind {
    /// A general back-reference.
    References,
    /// The source thought summarizes the target thought.
    Summarizes,
    /// The source thought corrects the target thought.
    Corrects,
    /// The source thought invalidates the target thought.
    Invalidates,
    /// The source thought was caused by the target thought.
    CausedBy,
    /// The source thought supports the target thought.
    Supports,
    /// The source thought contradicts the target thought.
    Contradicts,
    /// The source thought was derived from the target thought.
    DerivedFrom,
    /// The source thought continues the work or state of the target thought.
    ContinuesFrom,
    /// A generic semantic relation exists between source and target.
    RelatedTo,
    /// The source thought supersedes the target thought.
    ///
    /// Use when the source thought replaces a prior belief, plan, or fact
    /// without the prior being a clear *error* (use [`ThoughtRelationKind::Corrects`] or
    /// [`ThoughtRelationKind::Invalidates`] for errors).  The target thought is retained for
    /// audit; retrieval tooling should treat superseded thoughts as
    /// lower-priority.
    Supersedes,
}

/// Typed edge in the thought graph.
///
/// A relation explains why one thought points to another thought. This avoids
/// a common misconception: not every link is just a generic "reference". A
/// later thought may correct, summarize, support, or continue an earlier one,
/// and that semantic meaning matters during replay, inspection, and retrieval.
///
/// `ThoughtRelation` is more expressive than raw `refs`. Use `refs` when a
/// simple positional backlink is enough. Use relations when the meaning of the
/// link should survive into downstream tools, summaries, and audits.
///
/// # Example
///
/// ```
/// use mentisdb::{ThoughtRelation, ThoughtRelationKind};
/// use uuid::Uuid;
///
/// // Intra-chain relation (chain_key is None)
/// let intra = ThoughtRelation {
///     kind: ThoughtRelationKind::Supports,
///     target_id: Uuid::nil(),
///     chain_key: None,
/// };
/// assert_eq!(intra.kind, ThoughtRelationKind::Supports);
/// assert!(intra.chain_key.is_none());
///
/// // Cross-chain relation
/// let cross = ThoughtRelation {
///     kind: ThoughtRelationKind::Supersedes,
///     target_id: Uuid::nil(),
///     chain_key: Some("other-chain".to_string()),
/// };
/// assert_eq!(cross.chain_key.as_deref(), Some("other-chain"));
/// ```
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ThoughtRelation {
    /// Semantic meaning of the edge.
    pub kind: ThoughtRelationKind,
    /// Stable id of the target thought.
    pub target_id: Uuid,
    /// Optional chain key for cross-chain relations.
    ///
    /// When `None` (the default) this relation is intra-chain.
    /// When `Some(key)` this relation points to a thought on a different chain.
    ///
    /// In JSON output this field is omitted when `None` (backward compatible).
    /// In binary storage it is always written so that the sequential binary
    /// layout stays consistent across schema versions.
    #[serde(default)]
    pub chain_key: Option<String>,
}

impl Serialize for ThoughtRelation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if serializer.is_human_readable() {
            // Human-readable formats (JSON, TOML, etc.): omit `chain_key` when
            // `None` to keep output compact and backward compatible.
            let field_count = if self.chain_key.is_some() { 3 } else { 2 };
            let mut s = serializer.serialize_struct("ThoughtRelation", field_count)?;
            s.serialize_field("kind", &self.kind)?;
            s.serialize_field("target_id", &self.target_id)?;
            if let Some(ref ck) = self.chain_key {
                s.serialize_field("chain_key", ck)?;
            }
            s.end()
        } else {
            // Binary formats (bincode, etc.): always write every field so that
            // sequential decoding stays aligned regardless of the `chain_key`
            // value.  A `None` is encoded as a single zero byte.
            let mut s = serializer.serialize_struct("ThoughtRelation", 3)?;
            s.serialize_field("kind", &self.kind)?;
            s.serialize_field("target_id", &self.target_id)?;
            s.serialize_field("chain_key", &self.chain_key)?;
            s.end()
        }
    }
}

/// Builder-like input struct used to append rich thoughts.
///
/// `ThoughtInput` is the caller-authored description of a memory to be
/// committed. It is not yet part of the durable chain. Callers use it to say
/// what the thought means, how important it is, which earlier thoughts it
/// refers to, and which optional metadata should accompany it.
///
/// MentisDb then turns that input into a persisted [`Thought`] by adding
/// the system-managed fields that callers should not forge directly, such as:
///
/// - the stable thought `id`
/// - the chain `index`
/// - the commit `timestamp`
/// - the writer `agent_id`
/// - the `prev_hash`
/// - the final `hash`
///
/// In short:
///
/// - `ThoughtInput` is the proposed memory payload
/// - [`Thought`] is the committed memory record
///
/// Use `ThoughtInput` when you want richer metadata than the simple
/// [`MentisDb::append`] helper allows.
///
/// # Example
///
/// ```
/// use mentisdb::{ThoughtInput, ThoughtRole, ThoughtType};
///
/// let input = ThoughtInput::new(ThoughtType::Insight, "Rate limiting is the real bottleneck.")
///     .with_role(ThoughtRole::Summary)
///     .with_importance(0.9)
///     .with_tags(["api", "performance"]);
///
/// assert_eq!(input.thought_type, ThoughtType::Insight);
/// assert_eq!(input.role, ThoughtRole::Summary);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThoughtInput {
    /// Optional session identifier associated with the thought.
    ///
    /// This groups related thoughts from one run without changing the chain's
    /// durable identity.
    pub session_id: Option<Uuid>,
    /// Optional human-readable name of the producing agent.
    ///
    /// This populates the per-chain [`AgentRegistry`] entry for `agent_id`.
    pub agent_name: Option<String>,
    /// Optional owner or grouping label for the producing agent.
    ///
    /// Useful for shared chains, tenants, or human ownership models. This is
    /// stored in the agent registry rather than inline on every thought.
    pub agent_owner: Option<String>,
    /// Optional identifier of the key used to sign this thought payload.
    pub signing_key_id: Option<String>,
    /// Optional detached signature over the thought's signable payload.
    pub thought_signature: Option<Vec<u8>>,
    /// Semantic meaning of the thought.
    ///
    /// This answers "what kind of memory is this?"
    pub thought_type: ThoughtType,
    /// Operational role played by this thought.
    ///
    /// This answers "why is the system emitting or using this memory?"
    pub role: ThoughtRole,
    /// Primary human-readable content.
    ///
    /// This should be a durable memory statement, not hidden chain-of-thought.
    pub content: String,
    /// Optional confidence score between `0.0` and `1.0`.
    ///
    /// Use this when the content is uncertain or speculative.
    pub confidence: Option<f32>,
    /// Importance score between `0.0` and `1.0`.
    ///
    /// Higher values indicate memories that should matter more during
    /// retrieval, summarization, or pruning.
    pub importance: f32,
    /// Free-form tags for retrieval.
    ///
    /// Tags are lightweight labels supplied by the caller.
    pub tags: Vec<String>,
    /// Concept labels or semantic anchors for retrieval.
    ///
    /// Concepts are intended to be more semantic and reusable than ad hoc
    /// tags, though both can coexist.
    pub concepts: Vec<String>,
    /// Back-references to prior thought indices.
    ///
    /// These are compact positional links into the same chain.
    pub refs: Vec<u64>,
    /// Typed graph relations to prior thoughts.
    ///
    /// These preserve the meaning of the link, not just the fact that a link
    /// exists.
    pub relations: Vec<ThoughtRelation>,
}

impl ThoughtInput {
    /// Create a new input with default metadata.
    ///
    /// Defaults:
    /// - `role`: [`ThoughtRole::Memory`]
    /// - `importance`: `0.5`
    /// - `confidence`: `None`
    ///
    /// # Example
    ///
    /// ```
    /// use mentisdb::{ThoughtInput, ThoughtRole, ThoughtType};
    ///
    /// let input = ThoughtInput::new(ThoughtType::Plan, "Build a query index first.");
    ///
    /// assert_eq!(input.role, ThoughtRole::Memory);
    /// assert_eq!(input.importance, 0.5);
    /// ```
    pub fn new(thought_type: ThoughtType, content: impl Into<String>) -> Self {
        Self {
            session_id: None,
            agent_name: None,
            agent_owner: None,
            signing_key_id: None,
            thought_signature: None,
            thought_type,
            role: ThoughtRole::Memory,
            content: content.into(),
            confidence: None,
            importance: 0.5,
            tags: Vec::new(),
            concepts: Vec::new(),
            refs: Vec::new(),
            relations: Vec::new(),
        }
    }

    /// Attach a session identifier to the thought.
    pub fn with_session_id(mut self, session_id: Uuid) -> Self {
        self.session_id = Some(session_id);
        self
    }

    /// Attach a human-readable agent name to the thought.
    pub fn with_agent_name(mut self, agent_name: impl Into<String>) -> Self {
        self.agent_name = Some(agent_name.into());
        self
    }

    /// Attach an owner or grouping label to the thought.
    pub fn with_agent_owner(mut self, agent_owner: impl Into<String>) -> Self {
        self.agent_owner = Some(agent_owner.into());
        self
    }

    /// Attach the key identifier used to sign this thought payload.
    pub fn with_signing_key_id(mut self, signing_key_id: impl Into<String>) -> Self {
        self.signing_key_id = Some(signing_key_id.into());
        self
    }

    /// Attach a detached signature over the signable thought payload.
    pub fn with_thought_signature(mut self, thought_signature: Vec<u8>) -> Self {
        self.thought_signature = Some(thought_signature);
        self
    }

    /// Override the operational role of the thought.
    pub fn with_role(mut self, role: ThoughtRole) -> Self {
        self.role = role;
        self
    }

    /// Attach a confidence score between `0.0` and `1.0`.
    pub fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }

    /// Attach an importance score between `0.0` and `1.0`.
    pub fn with_importance(mut self, importance: f32) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    /// Replace the thought's tags.
    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Replace the thought's concept labels.
    pub fn with_concepts<I, S>(mut self, concepts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.concepts = concepts.into_iter().map(Into::into).collect();
        self
    }

    /// Add back-references to prior thought indices.
    pub fn with_refs(mut self, refs: Vec<u64>) -> Self {
        self.refs = refs;
        self
    }

    /// Add typed graph relations to prior thoughts.
    pub fn with_relations(mut self, relations: Vec<ThoughtRelation>) -> Self {
        self.relations = relations;
        self
    }

    /// Add a typed relation pointing to a thought on another chain.
    ///
    /// `chain_key` is the target chain's key.  `target_id` is the stable UUID
    /// of the thought on that chain.  Use the normal `with_relations` builder
    /// for intra-chain relations.
    pub fn with_cross_chain_relation(
        mut self,
        kind: ThoughtRelationKind,
        chain_key: impl Into<String>,
        target_id: Uuid,
    ) -> Self {
        self.relations.push(ThoughtRelation {
            kind,
            target_id,
            chain_key: Some(chain_key.into()),
        });
        self
    }
}

/// Render the deterministic signable payload for a proposed thought append.
///
/// This payload is intended to be signed by the producing agent before the
/// server commits the final thought record. It deliberately excludes
/// system-assigned fields such as `id`, `index`, `timestamp`, `prev_hash`, and
/// `hash`.
pub fn signable_thought_payload(agent_id: &str, input: &ThoughtInput) -> Vec<u8> {
    #[derive(Serialize)]
    struct SignableThoughtPayload<'a> {
        schema_version: u32,
        agent_id: &'a str,
        session_id: Option<Uuid>,
        thought_type: ThoughtType,
        role: ThoughtRole,
        content: &'a str,
        confidence: Option<f32>,
        importance: f32,
        tags: Vec<String>,
        concepts: Vec<String>,
        refs: &'a [u64],
        relations: &'a [ThoughtRelation],
    }

    let payload = SignableThoughtPayload {
        schema_version: MENTISDB_CURRENT_VERSION,
        agent_id,
        session_id: input.session_id,
        thought_type: input.thought_type,
        role: input.role,
        content: &input.content,
        confidence: input.confidence.map(|value| value.clamp(0.0, 1.0)),
        importance: input.importance.clamp(0.0, 1.0),
        tags: normalize_strings(input.tags.clone()),
        concepts: normalize_strings(input.concepts.clone()),
        refs: &input.refs,
        relations: &input.relations,
    };

    serde_json::to_vec(&payload).unwrap_or_default()
}

/// A single durable thought record.
///
/// `Thought` is the committed record that MentisDb stores and returns. It
/// contains the semantic memory payload together with the fields required for
/// ordering, attribution, and integrity verification.
///
/// A caller typically does not construct this type directly. Instead, the
/// caller provides a [`ThoughtInput`], and MentisDb produces a `Thought`
/// with system-managed fields filled in. This distinction prevents accidental
/// confusion between "memory content proposed by an agent" and "memory record
/// accepted into the chain".
///
/// # Example
///
/// ```rust,no_run
/// use std::path::PathBuf;
/// use mentisdb::{MentisDb, ThoughtType};
///
/// # fn main() -> std::io::Result<()> {
/// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_doc"), "agent1", "Agent", None, None)?;
/// let thought = chain.append("agent1", ThoughtType::Finding, "The cache hit rate is 97%.")?;
///
/// assert_eq!(thought.index, 0);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thought {
    /// Thought schema version used by this record.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    /// Stable unique identifier for this thought.
    ///
    /// This is the canonical target for future semantic relations.
    pub id: Uuid,
    /// Zero-based position within the chain.
    ///
    /// This reflects append order inside one chain. It is not a global ID.
    pub index: u64,
    /// UTC timestamp when the thought was recorded.
    ///
    /// Assigned at commit time by MentisDb.
    pub timestamp: DateTime<Utc>,
    /// Optional session identifier associated with the thought.
    pub session_id: Option<Uuid>,
    /// Stable identifier of the producing agent.
    ///
    /// This answers who wrote the record in a shared chain.
    pub agent_id: String,
    /// Optional identifier of the public key used to sign the thought payload.
    #[serde(default)]
    pub signing_key_id: Option<String>,
    /// Optional detached signature over the signable thought payload.
    #[serde(default)]
    pub thought_signature: Option<Vec<u8>>,
    /// Semantic meaning of the thought.
    pub thought_type: ThoughtType,
    /// Operational role played by this thought.
    pub role: ThoughtRole,
    /// Primary human-readable content.
    pub content: String,
    /// Optional confidence score between `0.0` and `1.0`.
    pub confidence: Option<f32>,
    /// Importance score between `0.0` and `1.0`.
    pub importance: f32,
    /// Free-form tags for retrieval.
    pub tags: Vec<String>,
    /// Concept labels or semantic anchors for retrieval.
    pub concepts: Vec<String>,
    /// Back-references to prior thought indices.
    pub refs: Vec<u64>,
    /// Typed graph relations to prior thoughts.
    pub relations: Vec<ThoughtRelation>,
    /// Hash of the previous thought in the chain.
    ///
    /// This links the record to the prior committed chain state.
    pub prev_hash: String,
    /// SHA-256 hash of this thought's canonical contents.
    ///
    /// This is the record's integrity fingerprint.
    pub hash: String,
}

/// Retrieval filter for semantic memory queries.
///
/// `ThoughtQuery` lets callers ask for slices of memory without replaying the
/// entire chain.
///
/// `ThoughtQuery` is read-only. It does not create or modify thoughts. Its job
/// is to select already-committed [`Thought`] records by semantic type,
/// operational role, agent identity, tags, concepts, text, confidence,
/// importance, and time range.
///
/// The relationship between the three main data shapes is:
///
/// - `ThoughtInput`: proposed memory to append
/// - `Thought`: committed durable memory
/// - `ThoughtQuery`: retrieval filter over committed memory
///
/// # Example
///
/// ```
/// use mentisdb::{ThoughtQuery, ThoughtType};
///
/// let query = ThoughtQuery::new()
///     .with_types(vec![ThoughtType::Decision, ThoughtType::Constraint])
///     .with_min_importance(0.8);
///
/// assert!(query.min_importance.is_some());
/// ```
#[derive(Debug, Clone, Default)]
pub struct ThoughtQuery {
    /// Semantic thought types to match.
    pub thought_types: Option<Vec<ThoughtType>>,
    /// Operational roles to match.
    pub roles: Option<Vec<ThoughtRole>>,
    /// Agent ids to match.
    pub agent_ids: Option<Vec<String>>,
    /// Agent names to match.
    pub agent_names: Option<Vec<String>>,
    /// Agent owners to match.
    pub agent_owners: Option<Vec<String>>,
    /// Match if any tag matches.
    pub tags_any: Vec<String>,
    /// Match if any concept matches.
    pub concepts_any: Vec<String>,
    /// Text filter applied to content, tags, and concepts.
    pub text_contains: Option<String>,
    /// Minimum importance threshold.
    pub min_importance: Option<f32>,
    /// Minimum confidence threshold.
    pub min_confidence: Option<f32>,
    /// Start of the timestamp window, inclusive.
    pub since: Option<DateTime<Utc>>,
    /// End of the timestamp window, inclusive.
    pub until: Option<DateTime<Utc>>,
    /// Maximum number of thoughts to return.
    pub limit: Option<usize>,
}

impl ThoughtQuery {
    /// Create an empty query that matches every thought.
    pub fn new() -> Self {
        Self::default()
    }

    /// Limit matches to the provided semantic thought types.
    pub fn with_types(mut self, thought_types: Vec<ThoughtType>) -> Self {
        self.thought_types = Some(thought_types);
        self
    }

    /// Limit matches to the provided thought roles.
    pub fn with_roles(mut self, roles: Vec<ThoughtRole>) -> Self {
        self.roles = Some(roles);
        self
    }

    /// Limit matches to the provided agent identifiers.
    pub fn with_agent_ids<I, S>(mut self, agent_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.agent_ids = Some(agent_ids.into_iter().map(Into::into).collect());
        self
    }

    /// Limit matches to the provided agent names.
    pub fn with_agent_names<I, S>(mut self, agent_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.agent_names = Some(agent_names.into_iter().map(Into::into).collect());
        self
    }

    /// Limit matches to the provided agent owner labels.
    pub fn with_agent_owners<I, S>(mut self, agent_owners: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.agent_owners = Some(agent_owners.into_iter().map(Into::into).collect());
        self
    }

    /// Match thoughts that have at least one of the provided tags.
    pub fn with_tags_any<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags_any = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Match thoughts that have at least one of the provided concepts.
    pub fn with_concepts_any<I, S>(mut self, concepts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.concepts_any = concepts.into_iter().map(Into::into).collect();
        self
    }

    /// Match thoughts whose content, tags, or concepts contain the provided text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text_contains = Some(text.into());
        self
    }

    /// Only match thoughts whose importance is at least this value.
    pub fn with_min_importance(mut self, importance: f32) -> Self {
        self.min_importance = Some(importance.clamp(0.0, 1.0));
        self
    }

    /// Only match thoughts whose confidence is at least this value.
    pub fn with_min_confidence(mut self, confidence: f32) -> Self {
        self.min_confidence = Some(confidence.clamp(0.0, 1.0));
        self
    }

    /// Only match thoughts at or after the given timestamp.
    pub fn with_since(mut self, since: DateTime<Utc>) -> Self {
        self.since = Some(since);
        self
    }

    /// Only match thoughts at or before the given timestamp.
    pub fn with_until(mut self, until: DateTime<Utc>) -> Self {
        self.until = Some(until);
        self
    }

    /// Apply an inclusive numeric time window expressed in Unix seconds or milliseconds.
    pub fn with_time_window(mut self, window: ThoughtTimeWindow) -> io::Result<Self> {
        let (since, until) = window.to_bounds()?;
        self.since = Some(since);
        self.until = Some(until);
        Ok(self)
    }

    /// Limit the number of returned thoughts.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    fn candidate_position_bounds(&self, thoughts: &[Thought]) -> (usize, usize) {
        let start = self
            .since
            .map(|since| thoughts.partition_point(|thought| thought.timestamp < since))
            .unwrap_or(0);
        let end = self
            .until
            .map(|until| thoughts.partition_point(|thought| thought.timestamp <= until))
            .unwrap_or(thoughts.len());

        (start.min(end), end)
    }

    fn matches(&self, thought: &Thought) -> bool {
        if let Some(types) = &self.thought_types {
            if !types.contains(&thought.thought_type) {
                return false;
            }
        }

        if let Some(roles) = &self.roles {
            if !roles.contains(&thought.role) {
                return false;
            }
        }

        if let Some(agent_ids) = &self.agent_ids {
            if !agent_ids
                .iter()
                .any(|agent_id| agent_id == &thought.agent_id)
            {
                return false;
            }
        }

        if !self.tags_any.is_empty()
            && !self
                .tags_any
                .iter()
                .any(|tag| contains_case_insensitive(&thought.tags, tag))
        {
            return false;
        }

        if !self.concepts_any.is_empty()
            && !self
                .concepts_any
                .iter()
                .any(|concept| contains_case_insensitive(&thought.concepts, concept))
        {
            return false;
        }

        self.matches_post_index_filters(thought)
    }

    fn matches_post_index_filters(&self, thought: &Thought) -> bool {
        if let Some(min_importance) = self.min_importance {
            if thought.importance < min_importance {
                return false;
            }
        }

        if let Some(min_confidence) = self.min_confidence {
            match thought.confidence {
                Some(confidence) if confidence >= min_confidence => {}
                _ => return false,
            }
        }

        if let Some(since) = self.since {
            if thought.timestamp < since {
                return false;
            }
        }

        if let Some(until) = self.until {
            if thought.timestamp > until {
                return false;
            }
        }

        true
    }
}

/// Ranking backend used to order ranked-search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RankedSearchBackend {
    /// In-process lexical scoring over thought and agent-registry text.
    Lexical,
    /// Lexical scoring blended with vector sidecar similarity.
    Hybrid,
    /// Lexical seed retrieval plus graph expansion over explicit refs and
    /// typed relations.
    LexicalGraph,
    /// Lexical and vector scoring blended with graph expansion over explicit
    /// refs and typed relations.
    HybridGraph,
    /// Metadata-only fallback when no ranked text query is provided.
    Heuristic,
}

impl RankedSearchBackend {
    /// Return the stable lowercase name of this backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Hybrid => "hybrid",
            Self::LexicalGraph => "lexical_graph",
            Self::HybridGraph => "hybrid_graph",
            Self::Heuristic => "heuristic",
        }
    }
}

/// Graph-expansion configuration for ranked search.
///
/// This mode is additive: it starts from lexical seed matches inside the
/// filtered candidate set, expands over `refs` and typed `relations`, then
/// reranks any reached candidate thoughts as supporting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RankedSearchGraph {
    /// Maximum graph distance explored from each lexical seed.
    pub max_depth: usize,
    /// Maximum number of unique graph nodes visited while expanding.
    pub max_visited: usize,
    /// Whether lexical seed thoughts should appear as depth-0 graph hits.
    pub include_seeds: bool,
    /// Direction policy used while traversing the thought graph.
    pub mode: crate::search::GraphExpansionMode,
}

impl RankedSearchGraph {
    /// Create a graph-expansion request with default limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum graph distance explored from each lexical seed.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the visit budget for expansion.
    pub fn with_max_visited(mut self, max_visited: usize) -> Self {
        self.max_visited = max_visited.max(1);
        self
    }

    /// Control whether lexical seeds are included in graph-expansion hits.
    pub fn with_include_seeds(mut self, include_seeds: bool) -> Self {
        self.include_seeds = include_seeds;
        self
    }

    /// Replace the graph traversal direction mode.
    pub fn with_mode(mut self, mode: crate::search::GraphExpansionMode) -> Self {
        self.mode = mode;
        self
    }
}

impl Default for RankedSearchGraph {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_visited: 128,
            include_seeds: true,
            mode: crate::search::GraphExpansionMode::Bidirectional,
        }
    }
}

/// Request for ranked retrieval over committed thoughts.
///
/// Ranked search is additive. The embedded [`ThoughtQuery`] still applies the
/// same filter semantics as [`MentisDb::query`]; ranked search only changes how
/// the matching candidates are ordered and trimmed.
#[derive(Debug, Clone)]
pub struct RankedSearchQuery {
    /// Deterministic semantic filter applied before ranked ordering.
    pub filter: ThoughtQuery,
    /// Optional lexical text query used for ranked scoring.
    pub text: Option<String>,
    /// Optional graph-expansion pass seeded from lexical matches.
    ///
    /// This configuration is only used when `text` normalizes to a non-empty
    /// lexical query. When `text` is absent or blank, ranked search falls back
    /// to metadata heuristics and graph expansion is ignored.
    pub graph: Option<RankedSearchGraph>,
    /// Maximum number of ranked hits to return.
    pub limit: usize,
}

impl RankedSearchQuery {
    /// Create an empty ranked-search request.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a deterministic semantic filter before ranking.
    pub fn with_filter(mut self, filter: ThoughtQuery) -> Self {
        self.filter = filter;
        self
    }

    /// Set the ranked lexical query text.
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Enable graph-aware ranked retrieval seeded from lexical matches.
    pub fn with_graph(mut self, graph: RankedSearchGraph) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Limit the number of ranked hits returned.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }
}

impl Default for RankedSearchQuery {
    fn default() -> Self {
        Self {
            filter: ThoughtQuery::new(),
            text: None,
            graph: None,
            limit: 10,
        }
    }
}

/// Score breakdown for one ranked-search hit.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RankedSearchScore {
    /// Score contributed by lexical matching.
    pub lexical: f32,
    /// Score contributed by vector-sidecar similarity.
    pub vector: f32,
    /// Score contributed by graph proximity to a lexical seed.
    pub graph: f32,
    /// Score contributed by semantic relation kinds along the chosen graph path.
    pub relation: f32,
    /// Score contributed by multiple distinct lexical seeds reaching this hit.
    pub seed_support: f32,
    /// Score contributed by the thought's importance value.
    pub importance: f32,
    /// Score contributed by the thought's confidence value.
    pub confidence: f32,
    /// Small recency tie-breaker derived from append order.
    pub recency: f32,
    /// Final combined score used for ranking.
    pub total: f32,
}

/// One ranked-search hit.
#[derive(Debug, Clone)]
pub struct RankedSearchHit<'a> {
    /// Matching thought.
    pub thought: &'a Thought,
    /// Score breakdown for this hit.
    pub score: RankedSearchScore,
    /// Graph distance from the lexical seed that surfaced this hit.
    pub graph_distance: Option<usize>,
    /// Number of distinct lexical seeds whose expansion reached this hit.
    pub graph_seed_paths: usize,
    /// Distinct relation kinds observed along supporting graph paths.
    pub graph_relation_kinds: Vec<ThoughtRelationKind>,
    /// Provenance path from the originating lexical seed to this hit.
    pub graph_path: Option<crate::search::GraphExpansionPath>,
    /// Unique normalized query terms that matched this hit.
    pub matched_terms: Vec<String>,
    /// Indexed field sources that contributed to the lexical score.
    pub match_sources: Vec<crate::search::lexical::LexicalMatchSource>,
}

/// Ranked-search response over committed thoughts.
#[derive(Debug, Clone)]
pub struct RankedSearchResult<'a> {
    /// Ranking backend used to score the hits.
    pub backend: RankedSearchBackend,
    /// Number of matching candidates considered after filtering and ranked-signal gating.
    pub total_candidates: usize,
    /// Top ranked hits in descending score order.
    pub hits: Vec<RankedSearchHit<'a>>,
}

/// Request for vector similarity retrieval over committed thoughts.
///
/// This API is additive. The embedded [`ThoughtQuery`] still applies the same
/// deterministic filter semantics as [`MentisDb::query`]; vector search only
/// reorders the eligible thoughts that already have embeddings in the selected
/// sidecar.
#[derive(Debug, Clone)]
pub struct VectorSearchQuery {
    /// Deterministic semantic filter applied before vector ranking.
    pub filter: ThoughtQuery,
    /// Query text to embed in the selected embedding space.
    pub text: String,
    /// Maximum number of hits to return.
    pub limit: usize,
    /// Optional floor for cosine similarity scores.
    pub min_score: Option<f32>,
}

impl VectorSearchQuery {
    /// Create an empty vector-search request.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            filter: ThoughtQuery::new(),
            text: text.into(),
            limit: 10,
            min_score: None,
        }
    }

    /// Apply a deterministic semantic filter before vector ranking.
    pub fn with_filter(mut self, filter: ThoughtQuery) -> Self {
        self.filter = filter;
        self
    }

    /// Limit the number of vector hits returned.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    /// Ignore hits below a minimum cosine similarity score.
    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = Some(min_score);
        self
    }
}

/// One vector-search hit.
#[derive(Debug, Clone)]
pub struct VectorSearchHit<'a> {
    /// Matching thought.
    pub thought: &'a Thought,
    /// Cosine similarity score for this hit.
    pub score: f32,
    /// Freshness state of the sidecar that produced this hit.
    pub freshness: crate::search::VectorSidecarFreshness,
}

/// Vector-search response over committed thoughts.
#[derive(Debug, Clone)]
pub struct VectorSearchResult<'a> {
    /// Embedding-space metadata used for the search.
    pub metadata: crate::search::EmbeddingMetadata,
    /// Freshness state of the sidecar used to serve this result.
    pub freshness: crate::search::VectorSidecarFreshness,
    /// Number of indexed candidates considered after filter application.
    pub total_candidates: usize,
    /// Top vector-ranked hits in descending cosine score order.
    pub hits: Vec<VectorSearchHit<'a>>,
}

/// Failure while rebuilding or querying a vector sidecar.
#[derive(Debug)]
pub enum VectorSearchError<E> {
    /// Building embeddings for the query or sidecar failed.
    Embedding(crate::search::EmbeddingBuildError<E>),
    /// Sidecar persistence failed.
    Io(io::Error),
    /// Vector-index validation or similarity search failed.
    Index(crate::search::VectorIndexError),
    /// The current chain does not expose stable persistence metadata.
    MissingPersistenceMetadata,
    /// No sidecar exists yet for the requested embedding space.
    MissingSidecar(PathBuf),
}

impl<E: fmt::Display> fmt::Display for VectorSearchError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Embedding(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Index(error) => write!(f, "{error}"),
            Self::MissingPersistenceMetadata => {
                write!(
                    f,
                    "this MentisDb handle does not expose stable persistence metadata"
                )
            }
            Self::MissingSidecar(path) => {
                write!(f, "no vector sidecar exists at {}", path.display())
            }
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for VectorSearchError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Embedding(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Index(error) => Some(error),
            Self::MissingPersistenceMetadata | Self::MissingSidecar(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
struct RankedGraphHit {
    best_hit: crate::search::GraphExpansionHit,
    seed_paths: usize,
    relation_kinds: Vec<ThoughtRelationKind>,
    relation_score: f32,
}

/// Direction for append-order thought traversal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThoughtTraversalDirection {
    /// Move from older thoughts toward newer thoughts.
    #[default]
    Forward,
    /// Move from newer thoughts toward older thoughts.
    Backward,
}

/// Stable locator for one committed thought.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ThoughtTraversalAnchor {
    /// Locate a thought by stable UUID.
    Id(Uuid),
    /// Locate a thought by stable chain hash.
    Hash(String),
    /// Locate a thought by append-order index.
    Index(u64),
    /// Locate the first thought in the chain.
    Genesis,
    /// Locate the newest thought at the chain tip.
    Head,
}

/// Unit for numeric time-window traversal filters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TimeWindowUnit {
    /// Numeric values are interpreted as Unix seconds.
    Seconds,
    /// Numeric values are interpreted as Unix milliseconds.
    Milliseconds,
}

/// Numeric time window used to derive inclusive `since`/`until` bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ThoughtTimeWindow {
    /// Inclusive window start in Unix seconds or milliseconds.
    pub start: i64,
    /// Non-negative window length in the same unit as `start`.
    pub delta: u64,
    /// Unit for `start` and `delta`.
    pub unit: TimeWindowUnit,
}

impl ThoughtTimeWindow {
    /// Convert this numeric window into inclusive UTC timestamp bounds.
    pub fn to_bounds(self) -> io::Result<(DateTime<Utc>, DateTime<Utc>)> {
        fn datetime_from_parts(seconds: i64, nanos: u32) -> io::Result<DateTime<Utc>> {
            DateTime::<Utc>::from_timestamp(seconds, nanos).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "time window falls outside the supported UTC timestamp range",
                )
            })
        }

        match self.unit {
            TimeWindowUnit::Seconds => {
                let until_seconds = self.start.checked_add(self.delta as i64).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "second-based time window overflows i64",
                    )
                })?;
                Ok((
                    datetime_from_parts(self.start, 0)?,
                    datetime_from_parts(until_seconds, 999_999_999)?,
                ))
            }
            TimeWindowUnit::Milliseconds => {
                let until_millis = self.start.checked_add(self.delta as i64).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "millisecond-based time window overflows i64",
                    )
                })?;
                let start_seconds = self.start.div_euclid(1_000);
                let start_nanos = (self.start.rem_euclid(1_000) as u32) * 1_000_000;
                let until_seconds = until_millis.div_euclid(1_000);
                let until_nanos = (until_millis.rem_euclid(1_000) as u32) * 1_000_000 + 999_999;
                Ok((
                    datetime_from_parts(start_seconds, start_nanos)?,
                    datetime_from_parts(until_seconds, until_nanos)?,
                ))
            }
        }
    }
}

/// Stable cursor for continuing append-order traversal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ThoughtTraversalCursor {
    /// Stable UUID of the referenced thought.
    pub id: Uuid,
    /// Append-order index of the referenced thought.
    pub index: u64,
    /// Stable content hash of the referenced thought.
    pub hash: String,
}

impl From<&Thought> for ThoughtTraversalCursor {
    fn from(thought: &Thought) -> Self {
        Self {
            id: thought.id,
            index: thought.index,
            hash: thought.hash.clone(),
        }
    }
}

/// Request for append-order traversal over committed thoughts.
#[derive(Debug, Clone)]
pub struct ThoughtTraversalRequest {
    /// Starting anchor for append-order traversal.
    pub anchor: ThoughtTraversalAnchor,
    /// Direction of sequential traversal.
    pub direction: ThoughtTraversalDirection,
    /// Whether to include the anchor thought when it matches the filter.
    pub include_anchor: bool,
    /// Maximum number of matching thoughts to return.
    pub chunk_size: usize,
    /// Semantic filters applied while traversing in append order.
    pub filter: ThoughtQuery,
}

impl ThoughtTraversalRequest {
    /// Create a traversal request with a chunk size and anchor.
    pub fn new(
        anchor: ThoughtTraversalAnchor,
        direction: ThoughtTraversalDirection,
        chunk_size: usize,
    ) -> Self {
        Self {
            anchor,
            direction,
            include_anchor: false,
            chunk_size,
            filter: ThoughtQuery::new(),
        }
    }

    /// Include the anchor thought in the returned chunk when it matches.
    pub fn with_include_anchor(mut self, include_anchor: bool) -> Self {
        self.include_anchor = include_anchor;
        self
    }

    /// Apply semantic filters while preserving traversal-specific chunking.
    pub fn with_filter(mut self, filter: ThoughtQuery) -> Self {
        self.filter = filter;
        self
    }
}

impl Default for ThoughtTraversalRequest {
    fn default() -> Self {
        Self {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 50,
            filter: ThoughtQuery::new(),
        }
    }
}

/// One page of append-order traversal results.
#[derive(Debug, Clone)]
pub struct ThoughtTraversalPage<'a> {
    /// Resolved anchor thought, if the chain was non-empty.
    pub anchor: Option<ThoughtTraversalCursor>,
    /// Matching thoughts in traversal order.
    pub thoughts: Vec<&'a Thought>,
    /// Whether additional matches exist in the requested traversal direction.
    pub has_more: bool,
    /// Cursor for continuing toward newer thoughts, if more matches exist.
    pub next_cursor: Option<ThoughtTraversalCursor>,
    /// Cursor for continuing toward older thoughts, if more matches exist.
    pub previous_cursor: Option<ThoughtTraversalCursor>,
}

#[derive(Default)]
struct QueryIndexes {
    by_agent_id: HashMap<String, Vec<usize>>,
    by_thought_type: HashMap<ThoughtType, Vec<usize>>,
    by_role: HashMap<ThoughtRole, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    by_concept: HashMap<String, Vec<usize>>,
}

impl QueryIndexes {
    fn from_thoughts(thoughts: &[Thought]) -> Self {
        let mut indexes = Self::default();
        for (position, thought) in thoughts.iter().enumerate() {
            indexes.observe(position, thought);
        }
        indexes
    }

    fn observe(&mut self, position: usize, thought: &Thought) {
        self.by_agent_id
            .entry(thought.agent_id.clone())
            .or_default()
            .push(position);
        self.by_thought_type
            .entry(thought.thought_type)
            .or_default()
            .push(position);
        self.by_role.entry(thought.role).or_default().push(position);
        for tag in &thought.tags {
            self.by_tag
                .entry(tag.to_lowercase())
                .or_default()
                .push(position);
        }
        for concept in &thought.concepts {
            self.by_concept
                .entry(concept.to_lowercase())
                .or_default()
                .push(position);
        }
    }
}

/// Append-only, hash-chained semantic memory store.
///
/// `MentisDb` stores thoughts in memory and persists them through a
/// [`StorageAdapter`]. Every record includes a SHA-256 hash of its canonical
/// contents plus the previous record hash, making offline tampering
/// detectable. The default backend is newline-delimited JSON via
/// [`BinaryStorageAdapter`].
///
/// # Example
///
/// ```rust,no_run
/// use std::path::PathBuf;
/// use mentisdb::{MentisDb, ThoughtType};
///
/// # fn main() -> std::io::Result<()> {
/// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_chain"), "researcher", "Researcher", None, None)?;
/// chain.append("researcher", ThoughtType::FactLearned, "The corpus contains 4 million rows.")?;
///
/// assert!(chain.verify_integrity());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
struct ChainPersistenceMetadata {
    chain_key: String,
    chain_dir: PathBuf,
    storage_kind: StorageAdapterKind,
}

trait ManagedEmbeddingProvider: Send + Sync {
    fn metadata(&self) -> &crate::search::EmbeddingMetadata;

    fn embed_documents(
        &self,
        inputs: &[crate::search::EmbeddingInput],
    ) -> io::Result<Vec<crate::search::VectorDocument>>;
}

struct RegisteredEmbeddingProvider<P> {
    provider: P,
}

impl<P> ManagedEmbeddingProvider for RegisteredEmbeddingProvider<P>
where
    P: crate::search::EmbeddingProvider + Send + Sync + 'static,
{
    fn metadata(&self) -> &crate::search::EmbeddingMetadata {
        self.provider.metadata()
    }

    fn embed_documents(
        &self,
        inputs: &[crate::search::EmbeddingInput],
    ) -> io::Result<Vec<crate::search::VectorDocument>> {
        crate::search::embed_batch_to_documents(&self.provider, inputs)
            .map_err(|error| io::Error::other(format!("Failed to build embeddings: {error}")))
    }
}

const MANAGED_VECTOR_SIDECAR_CONFIG_VERSION: u32 = 1;

/// Built-in provider kinds that MentisDB can manage persistently per chain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedVectorProviderKind {
    /// Deterministic in-process hashed text embeddings used by `mentisdbd`.
    LocalTextV1,
}

impl ManagedVectorProviderKind {
    fn key(self) -> &'static str {
        match self {
            Self::LocalTextV1 => "local-text-v1",
        }
    }

    fn metadata(self) -> crate::search::EmbeddingMetadata {
        <crate::search::LocalTextEmbeddingProvider as crate::search::EmbeddingProvider>::metadata(
            &self.provider(),
        )
        .clone()
    }

    fn provider(self) -> crate::search::LocalTextEmbeddingProvider {
        match self {
            Self::LocalTextV1 => crate::search::LocalTextEmbeddingProvider::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ManagedVectorSidecarConfig {
    pub provider_kind: ManagedVectorProviderKind,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ManagedVectorSidecarConfigFile {
    version: u32,
    providers: Vec<ManagedVectorSidecarConfig>,
}

impl Default for ManagedVectorSidecarConfigFile {
    fn default() -> Self {
        Self {
            version: MANAGED_VECTOR_SIDECAR_CONFIG_VERSION,
            providers: vec![ManagedVectorSidecarConfig {
                provider_kind: ManagedVectorProviderKind::LocalTextV1,
                enabled: true,
            }],
        }
    }
}

/// Runtime status for one persistently managed vector sidecar.
#[derive(Debug, Clone, Serialize)]
pub struct ManagedVectorSidecarStatus {
    /// Stable provider kind identifier.
    pub provider_kind: ManagedVectorProviderKind,
    /// Stable lowercase provider key used by dashboard and REST-style routes.
    pub provider_key: String,
    /// Whether append-time auto-sync is enabled for this provider on the chain.
    pub enabled: bool,
    /// Whether this open handle is currently managing the provider in memory.
    pub managed_in_memory: bool,
    /// Embedding-space metadata for the sidecar.
    pub metadata: crate::search::EmbeddingMetadata,
    /// Deterministic on-disk path of the sidecar file.
    pub sidecar_path: String,
    /// Whether the sidecar file exists on disk.
    pub sidecar_exists: bool,
    /// Freshness state of the sidecar relative to the current chain, if it
    /// could be loaded successfully.
    pub freshness: Option<crate::search::VectorSidecarFreshness>,
    /// Sidecar load error when the file exists but failed validation or parsing.
    pub load_error: Option<String>,
    /// Current thought count in the live chain.
    pub thought_count: usize,
    /// Number of thought vectors currently indexed in the sidecar, when loaded.
    pub indexed_thought_count: Option<usize>,
    /// Timestamp when the current sidecar file was generated, when loaded.
    pub generated_at: Option<DateTime<Utc>>,
}

/// Legacy `ThoughtRelation` used when reading schema-v0 binary chains.
///
/// Schema-v0 binary chains serialise relations as two-field records:
/// `(kind, target_id)`.  The modern [`ThoughtRelation`] adds `chain_key` as a
/// third field; reading old binary data with the three-field struct would
/// misalign the sequential decoder.  Migration code converts these into
/// canonical [`ThoughtRelation`] values with `chain_key: None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyThoughtRelation {
    kind: ThoughtRelationKind,
    target_id: Uuid,
}

impl From<LegacyThoughtRelation> for ThoughtRelation {
    fn from(l: LegacyThoughtRelation) -> Self {
        ThoughtRelation {
            kind: l.kind,
            target_id: l.target_id,
            chain_key: None,
        }
    }
}

fn merge_relation_kinds(
    existing: &mut Vec<ThoughtRelationKind>,
    additional: &[ThoughtRelationKind],
) {
    for kind in additional {
        if !existing.contains(kind) {
            existing.push(*kind);
        }
    }
}

/// Mirrors the `Thought` binary layout written by the 0.5.1 (schema-V1) daemon.
///
/// Field order MUST match the struct that was `#[derive(Serialize)]`d at write
/// time, because bincode encodes structs as positional sequences with no field
/// names.  Differences from the current [`Thought`]:
///
/// - Uses [`LegacyThoughtRelation`] (2-field) instead of the modern 3-field
///   [`ThoughtRelation`] so that the binary decoder stays aligned.
/// - `signing_key_id` and `thought_signature` are present but always `None` in
///   practice for V1 chains; they are preserved so the positional layout stays
///   correct during binary deserialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyThoughtV0 {
    schema_version: u32,
    id: Uuid,
    index: u64,
    timestamp: DateTime<Utc>,
    session_id: Option<Uuid>,
    agent_id: String,
    #[serde(default)]
    signing_key_id: Option<String>,
    #[serde(default)]
    thought_signature: Option<Vec<u8>>,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: String,
    confidence: Option<f32>,
    importance: f32,
    tags: Vec<String>,
    concepts: Vec<String>,
    refs: Vec<u64>,
    relations: Vec<LegacyThoughtRelation>,
    prev_hash: String,
    hash: String,
}

/// Append-only, hash-chained semantic memory store.
pub struct MentisDb {
    thoughts: Vec<Thought>,
    id_to_index: HashMap<Uuid, usize>,
    hash_to_index: HashMap<String, usize>,
    agent_registry: AgentRegistry,
    query_indexes: QueryIndexes,
    storage: Box<dyn StorageAdapter>,
    auto_flush: bool,
    persistence: Option<ChainPersistenceMetadata>,
    managed_vector_sidecars:
        HashMap<crate::search::EmbeddingMetadata, Box<dyn ManagedEmbeddingProvider>>,
    pending_agent_registry_sync: bool,
    pending_agent_registry_updates: usize,
    pending_chain_registration_sync: bool,
    pending_chain_registration_updates: usize,
}

impl MentisDb {
    /// Open or create a chain using the agent id as the durable storage key.
    ///
    /// The additional identity parameters are accepted for compatibility with
    /// `cloudllm`, but storage identity is now derived from `agent_id` so
    /// changing an agent's profile does not fork its memory file.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::MentisDb;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let chain = MentisDb::open(&PathBuf::from("/tmp/tc_open"), "agent1", "Agent", None, None)?;
    /// assert!(chain.thoughts().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn open(
        chain_dir: &PathBuf,
        agent_id: &str,
        _agent_name: &str,
        _expertise: Option<&str>,
        _personality: Option<&str>,
    ) -> io::Result<Self> {
        Self::open_with_key(chain_dir, agent_id)
    }

    /// Open or create a chain using a caller-provided storage adapter.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{JsonlStorageAdapter, MentisDb};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let adapter = JsonlStorageAdapter::for_chain_key(PathBuf::from("/tmp/tc_custom"), "project-memory");
    /// let chain = MentisDb::open_with_storage(Box::new(adapter))?;
    /// assert!(chain.thoughts().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_with_storage(storage: Box<dyn StorageAdapter>) -> io::Result<Self> {
        let thoughts = storage.load_thoughts()?;
        let persistence = derive_persistence_metadata(storage.as_ref());
        let mut agent_registry = if let Some(metadata) = &persistence {
            load_agent_registry(
                &metadata.chain_dir,
                &metadata.chain_key,
                metadata.storage_kind,
            )?
        } else {
            AgentRegistry::default()
        };

        let mut id_to_index = HashMap::new();
        let mut hash_to_index = HashMap::new();
        for (position, thought) in thoughts.iter().enumerate() {
            if thought.index != position as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Thought index {} does not match position {}",
                        thought.index, position
                    ),
                ));
            }
            id_to_index.insert(thought.id, position);
            hash_to_index.insert(thought.hash.clone(), position);
            agent_registry.observe(
                &thought.agent_id,
                None,
                None,
                thought.index,
                thought.timestamp,
            );
        }

        let chain = Self {
            query_indexes: QueryIndexes::from_thoughts(&thoughts),
            thoughts,
            id_to_index,
            hash_to_index,
            agent_registry,
            storage,
            auto_flush: true,
            persistence,
            managed_vector_sidecars: HashMap::new(),
            pending_agent_registry_sync: false,
            pending_agent_registry_updates: 0,
            pending_chain_registration_sync: false,
            pending_chain_registration_updates: 0,
        };

        if !chain.verify_integrity() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Thought chain integrity verification failed",
            ));
        }

        Ok(chain)
    }

    /// Open or create a chain using an explicit stable chain key.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::MentisDb;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let chain = MentisDb::open_with_key(PathBuf::from("/tmp/tc_key"), "project-memory")?;
    /// assert!(chain.thoughts().is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_with_key<P: AsRef<Path>>(chain_dir: P, chain_key: &str) -> io::Result<Self> {
        Self::open_with_key_and_storage_kind(chain_dir, chain_key, StorageAdapterKind::default())
    }

    /// Open or create a chain using an explicit stable chain key and default adapter preference.
    pub fn open_with_key_and_storage_kind<P: AsRef<Path>>(
        chain_dir: P,
        chain_key: &str,
        default_storage_kind: StorageAdapterKind,
    ) -> io::Result<Self> {
        fs::create_dir_all(chain_dir.as_ref())?;
        let storage_kind =
            resolve_storage_kind_for_chain(chain_dir.as_ref(), chain_key, default_storage_kind)?;
        let chain = Self::open_with_storage(storage_kind.for_chain_key(&chain_dir, chain_key))?;
        chain.persist_chain_registration()?;
        Ok(chain)
    }

    /// Append a simple thought with default metadata and no references.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{MentisDb, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_append"), "agent1", "Agent", None, None)?;
    /// let thought = chain.append("agent1", ThoughtType::Decision, "Use SQLite for local state.")?;
    ///
    /// assert_eq!(thought.index, 0);
    /// # Ok(())
    /// # }
    /// ```
    pub fn append(
        &mut self,
        agent_id: &str,
        thought_type: ThoughtType,
        content: &str,
    ) -> io::Result<&Thought> {
        self.append_thought(agent_id, ThoughtInput::new(thought_type, content))
    }

    /// Append a simple thought that references prior thought indices.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{MentisDb, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_refs"), "agent1", "Agent", None, None)?;
    /// chain.append("agent1", ThoughtType::Finding, "Observed rising latency.")?;
    /// let summary = chain.append_with_refs("agent1", ThoughtType::Summary, "Latency issue captured.", vec![0])?;
    ///
    /// assert_eq!(summary.refs, vec![0]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn append_with_refs(
        &mut self,
        agent_id: &str,
        thought_type: ThoughtType,
        content: &str,
        refs: Vec<u64>,
    ) -> io::Result<&Thought> {
        self.append_thought(
            agent_id,
            ThoughtInput::new(thought_type, content).with_refs(refs),
        )
    }

    /// Append a rich thought with semantic metadata, tags, concepts, and relations.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{MentisDb, ThoughtInput, ThoughtRole, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_rich"), "agent1", "Agent", None, None)?;
    /// let input = ThoughtInput::new(ThoughtType::Constraint, "The system must work offline.")
    ///     .with_role(ThoughtRole::Checkpoint)
    ///     .with_importance(0.95)
    ///     .with_tags(["offline", "ops"]);
    /// chain.append_thought("agent1", input)?;
    ///
    /// assert_eq!(chain.thoughts().len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn append_thought(
        &mut self,
        agent_id: &str,
        mut input: ThoughtInput,
    ) -> io::Result<&Thought> {
        validate_refs(&self.thoughts, &input.refs)?;

        let mut relations = input.relations.clone();
        for &reference_index in &input.refs {
            if let Some(target) = self.thoughts.get(reference_index as usize) {
                relations.push(ThoughtRelation {
                    kind: ThoughtRelationKind::References,
                    target_id: target.id,
                    chain_key: None,
                });
            }
        }
        dedupe_relations(&mut relations);

        let index = self.thoughts.len() as u64;
        let prev_hash = self
            .thoughts
            .last()
            .map(|thought| thought.hash.clone())
            .unwrap_or_default();
        let timestamp = Utc::now();
        let display_name = input
            .agent_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned);
        let owner = input
            .agent_owner
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let previous_agent_count = self.agent_registry.agents.len();
        input.importance = input.importance.clamp(0.0, 1.0);
        let thought = Thought {
            schema_version: MENTISDB_CURRENT_VERSION,
            id: Uuid::new_v4(),
            index,
            timestamp,
            session_id: input.session_id,
            agent_id: agent_id.to_string(),
            signing_key_id: input
                .signing_key_id
                .take()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            thought_signature: input.thought_signature.take(),
            thought_type: input.thought_type,
            role: input.role,
            content: input.content,
            confidence: input.confidence.map(|value| value.clamp(0.0, 1.0)),
            importance: input.importance,
            tags: normalize_strings(input.tags),
            concepts: normalize_strings(input.concepts),
            refs: input.refs,
            relations,
            prev_hash,
            hash: String::new(),
        };

        let hash = compute_thought_hash(&thought);
        let thought = Thought { hash, ..thought };

        self.storage.append_thought(&thought)?;

        self.agent_registry.observe(
            agent_id,
            display_name.as_deref(),
            owner.as_deref(),
            index,
            timestamp,
        );
        let agent_count_changed = self.agent_registry.agents.len() != previous_agent_count;
        // Insert into all in-memory indexes before pushing so that
        // self.thoughts.len() still equals `index` (the correct 0-based position).
        self.id_to_index.insert(thought.id, self.thoughts.len());
        self.hash_to_index
            .insert(thought.hash.clone(), self.thoughts.len());
        self.query_indexes.observe(self.thoughts.len(), &thought);
        self.thoughts.push(thought.clone());
        self.sync_managed_vector_sidecars_for_append(self.thoughts.last().unwrap())?;
        self.mark_agent_registry_dirty();
        self.maybe_flush_agent_registry(
            self.auto_flush || self.thoughts.len() == 1 || agent_count_changed,
        )?;
        self.mark_chain_registration_dirty();
        self.maybe_flush_chain_registration(self.thoughts.len() == 1 || agent_count_changed)?;
        Ok(self.thoughts.last().unwrap())
    }

    /// Verify the entire hash chain and sequence invariants.
    ///
    /// Returns `false` if:
    /// - any `index` does not match its position
    /// - any `prev_hash` does not match the previous thought hash
    /// - any thought hash does not match its recomputed canonical hash
    pub fn verify_integrity(&self) -> bool {
        let mut prev_hash = String::new();
        for (position, thought) in self.thoughts.iter().enumerate() {
            if thought.index != position as u64 {
                return false;
            }
            if thought.prev_hash != prev_hash {
                return false;
            }
            if thought.hash != compute_thought_hash(thought) {
                return false;
            }
            prev_hash = thought.hash.clone();
        }
        true
    }

    /// Resolve all context reachable from the target thought index.
    ///
    /// Traversal follows both explicit `refs` and typed relations.
    pub fn resolve_context(&self, target_index: u64) -> Vec<&Thought> {
        let Some(target) = self.thoughts.get(target_index as usize) else {
            return Vec::new();
        };
        self.resolve_context_by_id(target.id)
    }

    /// Resolve all context reachable from the target thought id.
    pub fn resolve_context_by_id(&self, target_id: Uuid) -> Vec<&Thought> {
        let mut visited = HashSet::new();
        let mut stack = vec![target_id];

        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }

            if let Some(&position) = self.id_to_index.get(&id) {
                let thought = &self.thoughts[position];
                for relation in &thought.relations {
                    if !visited.contains(&relation.target_id) {
                        stack.push(relation.target_id);
                    }
                }
                for &reference_index in &thought.refs {
                    if let Some(reference) = self.thoughts.get(reference_index as usize) {
                        if !visited.contains(&reference.id) {
                            stack.push(reference.id);
                        }
                    }
                }
            }
        }

        let mut resolved: Vec<&Thought> = visited
            .into_iter()
            .filter_map(|id| self.id_to_index.get(&id).copied())
            .map(|position| &self.thoughts[position])
            .collect();
        resolved.sort_by_key(|thought| thought.index);
        resolved
    }

    /// Return the first thought in append order, if any.
    pub fn genesis_thought(&self) -> Option<&Thought> {
        self.thoughts.first()
    }

    /// Return the newest thought at the current chain tip, if any.
    pub fn head_thought(&self) -> Option<&Thought> {
        self.thoughts.last()
    }

    /// Return one thought by append-order index.
    pub fn get_thought_by_index(&self, index: u64) -> Option<&Thought> {
        self.thoughts.get(index as usize)
    }

    /// Return one thought by stable UUID.
    pub fn get_thought_by_id(&self, thought_id: Uuid) -> Option<&Thought> {
        self.id_to_index
            .get(&thought_id)
            .copied()
            .map(|position| &self.thoughts[position])
    }

    /// Return one thought by stable chain hash.
    pub fn get_thought_by_hash(&self, hash: &str) -> Option<&Thought> {
        self.hash_to_index
            .get(hash)
            .copied()
            .map(|position| &self.thoughts[position])
    }

    /// Resolve one thought locator to a committed thought.
    pub fn get_thought(&self, anchor: &ThoughtTraversalAnchor) -> Option<&Thought> {
        match anchor {
            ThoughtTraversalAnchor::Id(thought_id) => self.get_thought_by_id(*thought_id),
            ThoughtTraversalAnchor::Hash(hash) => self.get_thought_by_hash(hash),
            ThoughtTraversalAnchor::Index(index) => self.get_thought_by_index(*index),
            ThoughtTraversalAnchor::Genesis => self.genesis_thought(),
            ThoughtTraversalAnchor::Head => self.head_thought(),
        }
    }

    /// Traverse thoughts in append order from an anchor with optional filters.
    ///
    /// Results are returned in traversal order: forward traversal yields
    /// increasing append-order indexes, while backward traversal yields
    /// decreasing indexes.
    pub fn traverse_thoughts(
        &self,
        request: &ThoughtTraversalRequest,
    ) -> io::Result<ThoughtTraversalPage<'_>> {
        if request.chunk_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk_size must be greater than zero",
            ));
        }

        let anchor_position = self.resolve_traversal_anchor_position(request);
        let anchor =
            anchor_position.map(|position| ThoughtTraversalCursor::from(&self.thoughts[position]));
        let Some(anchor_position) = anchor_position else {
            return Ok(ThoughtTraversalPage {
                anchor,
                thoughts: Vec::new(),
                has_more: false,
                next_cursor: None,
                previous_cursor: None,
            });
        };

        let mut filter = request.filter.clone();
        filter.limit = None;
        let (start, end) = filter.candidate_position_bounds(&self.thoughts);
        let mut positions =
            if let Some(candidate_positions) = self.indexed_candidate_positions(&filter) {
                let bounded_positions: Vec<usize> = candidate_positions
                    .into_iter()
                    .filter(|position| *position >= start && *position < end)
                    .collect();
                self.collect_traversal_matches_from_candidates(
                    &bounded_positions,
                    anchor_position,
                    request.direction,
                    request.include_anchor,
                    request.chunk_size,
                    &filter,
                )
            } else {
                self.collect_traversal_matches_linear(
                    anchor_position,
                    request.direction,
                    request.include_anchor,
                    request.chunk_size,
                    &filter,
                )
            };
        let has_more = positions.len() > request.chunk_size;
        if has_more {
            positions.pop();
        }

        let thoughts = positions
            .iter()
            .map(|&position| &self.thoughts[position])
            .collect::<Vec<_>>();

        let (next_cursor, previous_cursor) = if let (Some(first_position), Some(last_position)) =
            (positions.first().copied(), positions.last().copied())
        {
            (
                Some(ThoughtTraversalCursor::from(&self.thoughts[last_position])),
                Some(ThoughtTraversalCursor::from(&self.thoughts[first_position])),
            )
        } else {
            (None, None)
        };

        Ok(ThoughtTraversalPage {
            anchor,
            thoughts,
            has_more,
            next_cursor,
            previous_cursor,
        })
    }

    /// Return the per-chain registry of known agents.
    pub fn agent_registry(&self) -> &AgentRegistry {
        &self.agent_registry
    }

    /// Return one registered agent record by stable `agent_id`.
    pub fn get_agent(&self, agent_id: &str) -> Option<&AgentRecord> {
        self.agent_registry.agents.get(agent_id)
    }

    /// Return the full per-chain agent registry as an ordered list of records.
    pub fn list_agent_registry(&self) -> Vec<&AgentRecord> {
        self.agent_registry.agents.values().collect()
    }

    /// Create or update a durable agent record in the per-chain registry.
    ///
    /// This allows callers to register agents before they write thoughts or to
    /// enrich existing registry entries with descriptive metadata.
    pub fn upsert_agent(
        &mut self,
        agent_id: &str,
        display_name: Option<&str>,
        owner: Option<&str>,
        description: Option<&str>,
        status: Option<AgentStatus>,
    ) -> io::Result<AgentRecord> {
        let agent_id = normalize_non_empty_label(agent_id, "agent_id")?;
        let record = self
            .agent_registry
            .agents
            .entry(agent_id.clone())
            .or_insert_with(|| AgentRecord::stub(&agent_id));
        if let Some(display_name) = display_name {
            record.set_display_name(display_name);
        }
        if owner.is_some() {
            record.set_owner(owner);
        }
        if description.is_some() {
            record.set_description(description);
        }
        if let Some(status) = status {
            record.status = status;
        }
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    /// Set or clear the free-form description of one registered agent.
    pub fn set_agent_description(
        &mut self,
        agent_id: &str,
        description: Option<&str>,
    ) -> io::Result<AgentRecord> {
        let record = self.agent_record_mut(agent_id)?;
        record.set_description(description);
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    /// Add one alias to an existing registered agent.
    pub fn add_agent_alias(&mut self, agent_id: &str, alias: &str) -> io::Result<AgentRecord> {
        let alias = normalize_non_empty_label(alias, "alias")?;
        let record = self.agent_record_mut(agent_id)?;
        record.add_alias(&alias);
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    /// Add or replace one public verification key on an existing registered agent.
    pub fn add_agent_key(
        &mut self,
        agent_id: &str,
        key_id: &str,
        algorithm: PublicKeyAlgorithm,
        public_key_bytes: Vec<u8>,
    ) -> io::Result<AgentRecord> {
        let key_id = normalize_non_empty_label(key_id, "key_id")?;
        if public_key_bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "public_key_bytes must not be empty",
            ));
        }

        let record = self.agent_record_mut(agent_id)?;
        record.add_public_key(AgentPublicKey {
            key_id,
            algorithm,
            public_key_bytes,
            added_at: Utc::now(),
            revoked_at: None,
        });
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    /// Revoke one public verification key on an existing registered agent.
    pub fn revoke_agent_key(&mut self, agent_id: &str, key_id: &str) -> io::Result<AgentRecord> {
        let key_id = normalize_non_empty_label(key_id, "key_id")?;
        let record = self.agent_record_mut(agent_id)?;
        if !record.revoke_key(&key_id, Utc::now()) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No key '{key_id}' found for agent '{agent_id}'"),
            ));
        }
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    /// Mark one registered agent as disabled.
    pub fn disable_agent(&mut self, agent_id: &str) -> io::Result<AgentRecord> {
        let record = self.agent_record_mut(agent_id)?;
        record.status = AgentStatus::Revoked;
        let updated = record.clone();
        self.persist_registries()?;
        Ok(updated)
    }

    fn agent_record_for(&self, agent_id: &str) -> Option<&AgentRecord> {
        self.agent_registry.agents.get(agent_id)
    }

    fn agent_record_mut(&mut self, agent_id: &str) -> io::Result<&mut AgentRecord> {
        let agent_id = normalize_non_empty_label(agent_id, "agent_id")?;
        self.agent_registry
            .agents
            .get_mut(&agent_id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No agent '{agent_id}' is registered in this chain"),
                )
            })
    }

    fn agent_label_for(&self, thought: &Thought) -> String {
        let mut label = if let Some(record) = self.agent_record_for(&thought.agent_id) {
            if record.display_name.trim().is_empty() || record.display_name == thought.agent_id {
                thought.agent_id.clone()
            } else {
                format!("{} [{}]", record.display_name, thought.agent_id)
            }
        } else {
            thought.agent_id.clone()
        };

        if let Some(owner) = self
            .agent_record_for(&thought.agent_id)
            .and_then(|record| record.owner.as_ref())
            .filter(|owner| !owner.trim().is_empty())
        {
            label.push_str(&format!(" owned by {}", owner));
        }

        label
    }

    fn query_matches_registry(&self, thought: &Thought, query: &ThoughtQuery) -> bool {
        if let Some(agent_names) = &query.agent_names {
            let Some(record) = self.agent_record_for(&thought.agent_id) else {
                return false;
            };
            let matched = agent_names.iter().any(|agent_name| {
                equals_case_insensitive(&record.display_name, agent_name)
                    || record
                        .aliases
                        .iter()
                        .any(|alias| equals_case_insensitive(alias, agent_name))
            });
            if !matched {
                return false;
            }
        }

        if let Some(agent_owners) = &query.agent_owners {
            let Some(owner) = self
                .agent_record_for(&thought.agent_id)
                .and_then(|record| record.owner.as_ref())
            else {
                return false;
            };
            if !agent_owners
                .iter()
                .any(|agent_owner| equals_case_insensitive(owner, agent_owner))
            {
                return false;
            }
        }

        if let Some(text) = &query.text_contains {
            let needle = text.to_lowercase();
            let registry_text_match = self
                .agent_record_for(&thought.agent_id)
                .map(|record| {
                    record.display_name.to_lowercase().contains(&needle)
                        || record
                            .owner
                            .as_ref()
                            .map(|owner| owner.to_lowercase().contains(&needle))
                            .unwrap_or(false)
                        || record
                            .aliases
                            .iter()
                            .any(|alias| alias.to_lowercase().contains(&needle))
                        || record
                            .description
                            .as_ref()
                            .map(|description| description.to_lowercase().contains(&needle))
                            .unwrap_or(false)
                })
                .unwrap_or(false);

            if !registry_text_match
                && !thought.content.to_lowercase().contains(&needle)
                && !thought.agent_id.to_lowercase().contains(&needle)
                && !thought
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle))
                && !thought
                    .concepts
                    .iter()
                    .any(|concept| concept.to_lowercase().contains(&needle))
            {
                return false;
            }
        }

        true
    }

    fn thought_matches_query(&self, thought: &Thought, query: &ThoughtQuery) -> bool {
        query.matches(thought) && self.query_matches_registry(thought, query)
    }

    fn thought_matches_indexed_query(&self, thought: &Thought, query: &ThoughtQuery) -> bool {
        query.matches_post_index_filters(thought) && self.query_matches_registry(thought, query)
    }

    fn resolve_traversal_anchor_position(
        &self,
        request: &ThoughtTraversalRequest,
    ) -> Option<usize> {
        if self.thoughts.is_empty() {
            return None;
        }

        self.locate_thought_position(&request.anchor)
    }

    fn locate_thought_position(&self, anchor: &ThoughtTraversalAnchor) -> Option<usize> {
        match anchor {
            ThoughtTraversalAnchor::Id(thought_id) => self.id_to_index.get(thought_id).copied(),
            ThoughtTraversalAnchor::Hash(hash) => self.hash_to_index.get(hash).copied(),
            ThoughtTraversalAnchor::Index(index) => {
                self.thoughts.get(*index as usize).map(|_| *index as usize)
            }
            ThoughtTraversalAnchor::Genesis => self.thoughts.first().map(|_| 0),
            ThoughtTraversalAnchor::Head => self.thoughts.len().checked_sub(1),
        }
    }

    fn step_position(
        &self,
        position: usize,
        direction: ThoughtTraversalDirection,
    ) -> Option<usize> {
        match direction {
            ThoughtTraversalDirection::Forward => {
                let next = position + 1;
                (next < self.thoughts.len()).then_some(next)
            }
            ThoughtTraversalDirection::Backward => position.checked_sub(1),
        }
    }

    fn collect_traversal_matches_linear(
        &self,
        anchor_position: usize,
        direction: ThoughtTraversalDirection,
        include_anchor: bool,
        chunk_size: usize,
        query: &ThoughtQuery,
    ) -> Vec<usize> {
        let mut matches = Vec::with_capacity(chunk_size.saturating_add(1));
        let mut current = if include_anchor {
            Some(anchor_position)
        } else {
            self.step_position(anchor_position, direction)
        };

        while let Some(candidate) = current {
            let thought = &self.thoughts[candidate];
            if self.thought_matches_query(thought, query) {
                matches.push(candidate);
                if matches.len() > chunk_size {
                    break;
                }
            }
            current = self.step_position(candidate, direction);
        }

        matches
    }

    fn collect_traversal_matches_from_candidates(
        &self,
        candidates: &[usize],
        anchor_position: usize,
        direction: ThoughtTraversalDirection,
        include_anchor: bool,
        chunk_size: usize,
        query: &ThoughtQuery,
    ) -> Vec<usize> {
        let mut matches = Vec::with_capacity(chunk_size.saturating_add(1));

        match direction {
            ThoughtTraversalDirection::Forward => {
                let start_index = match candidates.binary_search(&anchor_position) {
                    Ok(index) if include_anchor => index,
                    Ok(index) => index + 1,
                    Err(index) => index,
                };
                for &candidate in candidates.iter().skip(start_index) {
                    if self.thought_matches_indexed_query(&self.thoughts[candidate], query) {
                        matches.push(candidate);
                        if matches.len() > chunk_size {
                            break;
                        }
                    }
                }
            }
            ThoughtTraversalDirection::Backward => {
                let end_index = match candidates.binary_search(&anchor_position) {
                    Ok(index) if include_anchor => index + 1,
                    Ok(index) => index,
                    Err(index) => index,
                };
                for &candidate in candidates[..end_index].iter().rev() {
                    if self.thought_matches_indexed_query(&self.thoughts[candidate], query) {
                        matches.push(candidate);
                        if matches.len() > chunk_size {
                            break;
                        }
                    }
                }
            }
        }

        matches
    }

    /// Render a JSON representation of a thought with resolved agent metadata.
    pub fn thought_json(&self, thought: &Thought) -> serde_json::Value {
        let agent_record = self.agent_record_for(&thought.agent_id);
        serde_json::json!({
            "schema_version": thought.schema_version,
            "id": thought.id,
            "index": thought.index,
            "timestamp": thought.timestamp,
            "session_id": thought.session_id,
            "agent_id": thought.agent_id,
            "agent_name": agent_record.map(|record| record.display_name.clone()).unwrap_or_else(|| thought.agent_id.clone()),
            "agent_owner": agent_record.and_then(|record| record.owner.clone()),
            "signing_key_id": thought.signing_key_id,
            "thought_signature": thought.thought_signature,
            "thought_type": thought.thought_type,
            "role": thought.role,
            "content": thought.content,
            "confidence": thought.confidence,
            "importance": thought.importance,
            "tags": thought.tags,
            "concepts": thought.concepts,
            "refs": thought.refs,
            "relations": thought.relations,
            "prev_hash": thought.prev_hash,
            "hash": thought.hash,
        })
    }

    /// Query the chain using semantic filters.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{MentisDb, ThoughtQuery, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_query"), "agent1", "Agent", None, None)?;
    /// chain.append("agent1", ThoughtType::Decision, "Use SQLite for local state.")?;
    ///
    /// let results = chain.query(&ThoughtQuery::new().with_types(vec![ThoughtType::Decision]));
    /// assert_eq!(results.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn query(&self, query: &ThoughtQuery) -> Vec<&Thought> {
        let (start, end) = query.candidate_position_bounds(&self.thoughts);
        let empty: &[Thought] = &[];
        let candidate_positions = self.indexed_candidate_positions(query);
        let indexed_filters_applied = candidate_positions.is_some();
        let candidate_thoughts: Box<dyn Iterator<Item = &Thought> + '_> =
            if let Some(positions) = candidate_positions {
                Box::new(
                    positions
                        .into_iter()
                        .filter(move |position| *position >= start && *position < end)
                        .map(|position| &self.thoughts[position]),
                )
            } else {
                let slice = if start >= end {
                    empty
                } else {
                    &self.thoughts[start..end]
                };
                Box::new(slice.iter())
            };

        let mut results: Vec<&Thought> = candidate_thoughts
            .filter(|thought| {
                let thought_matches = if indexed_filters_applied {
                    query.matches_post_index_filters(thought)
                } else {
                    query.matches(thought)
                };
                thought_matches && self.query_matches_registry(thought, query)
            })
            .collect();

        if let Some(limit) = query.limit {
            if results.len() > limit {
                results = results[results.len() - limit..].to_vec();
            }
        }

        results
    }

    /// Query the chain with ranked ordering over the filtered candidates.
    ///
    /// Ranked search is additive: it first applies the embedded
    /// [`ThoughtQuery`] with the same semantics as [`MentisDb::query`], then
    /// reorders the matching candidates by lexical score, optional vector
    /// sidecar similarity, optional graph-expansion proximity, or lightweight
    /// metadata heuristics.
    pub fn query_ranked(&self, request: &RankedSearchQuery) -> RankedSearchResult<'_> {
        let candidates = self.query(&request.filter);
        let ranked_text = request
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let lexical_scores = ranked_text
            .map(|text| self.rank_candidates_lexically(&candidates, text))
            .unwrap_or_default();
        let vector_scores = ranked_text
            .map(|text| self.rank_candidates_semantically(&candidates, text))
            .unwrap_or_default();
        let graph_scores = ranked_text
            .and(request.graph.as_ref())
            .map(|graph| self.expand_ranked_candidates(&candidates, graph, &lexical_scores))
            .unwrap_or_default();
        let backend =
            if ranked_text.is_some() && request.graph.is_some() && !vector_scores.is_empty() {
                RankedSearchBackend::HybridGraph
            } else if ranked_text.is_some() && request.graph.is_some() {
                RankedSearchBackend::LexicalGraph
            } else if ranked_text.is_some() && !vector_scores.is_empty() {
                RankedSearchBackend::Hybrid
            } else if ranked_text.is_some() {
                RankedSearchBackend::Lexical
            } else {
                RankedSearchBackend::Heuristic
            };
        let mut hits: Vec<RankedSearchHit<'_>> = candidates
            .into_iter()
            .filter_map(|thought| {
                self.rank_search_hit(
                    thought,
                    ranked_text,
                    &lexical_scores,
                    &vector_scores,
                    &graph_scores,
                )
            })
            .collect();
        let total_candidates = hits.len();

        hits.sort_by(|left, right| {
            right
                .score
                .total
                .total_cmp(&left.score.total)
                .then_with(|| right.score.lexical.total_cmp(&left.score.lexical))
                .then_with(|| right.score.vector.total_cmp(&left.score.vector))
                .then_with(|| right.score.graph.total_cmp(&left.score.graph))
                .then_with(|| right.score.relation.total_cmp(&left.score.relation))
                .then_with(|| right.score.seed_support.total_cmp(&left.score.seed_support))
                .then_with(|| right.thought.importance.total_cmp(&left.thought.importance))
                .then_with(|| {
                    right
                        .thought
                        .confidence
                        .unwrap_or_default()
                        .total_cmp(&left.thought.confidence.unwrap_or_default())
                })
                .then_with(|| right.thought.index.cmp(&left.thought.index))
        });

        if hits.len() > request.limit {
            hits.truncate(request.limit);
        }

        RankedSearchResult {
            backend,
            total_candidates,
            hits,
        }
    }

    /// Query the chain and return grouped context bundles anchored on lexical seeds.
    ///
    /// This is the grouped counterpart to [`MentisDb::query_ranked`]. It keeps
    /// the same deterministic filter semantics as [`ThoughtQuery`], derives
    /// lexical seeds from the ranked text query, then groups supporting
    /// graph-expanded context beneath each seed in lexical rank order.
    ///
    /// When `request.graph` is `None`, bundles still return lexical seed
    /// ordering but contain no supporting graph hits.
    pub fn query_context_bundles(
        &self,
        request: &RankedSearchQuery,
    ) -> crate::search::ContextBundleResult {
        let candidates = self.query(&request.filter);
        let ranked_text = request
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let Some(ranked_text) = ranked_text else {
            return crate::search::ContextBundleResult {
                bundles: Vec::new(),
                consumed_hits: 0,
            };
        };

        let lexical_scores = self.rank_candidates_lexically(&candidates, ranked_text);
        let seeds = self.context_bundle_seeds(&lexical_scores, request.limit);
        if seeds.is_empty() {
            return crate::search::ContextBundleResult {
                bundles: Vec::new(),
                consumed_hits: 0,
            };
        }

        let graph_hits = request
            .graph
            .as_ref()
            .map(|graph| self.expand_ranked_candidate_paths(&candidates, graph, &lexical_scores))
            .unwrap_or_default();

        crate::search::build_context_bundles(
            &seeds,
            &graph_hits,
            crate::search::ContextBundleOptions::default(),
        )
    }

    /// Return the deterministic on-disk path for one vector sidecar.
    pub fn vector_sidecar_path(
        &self,
        metadata: &crate::search::EmbeddingMetadata,
    ) -> io::Result<PathBuf> {
        let Some(persistence) = &self.persistence else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this MentisDb handle does not expose stable persistence metadata",
            ));
        };

        Ok(chain_vector_sidecar_path(
            &persistence.chain_dir,
            &persistence.chain_key,
            persistence.storage_kind,
            metadata,
        ))
    }

    /// Load and integrity-check one vector sidecar for the requested embedding space.
    pub fn load_vector_sidecar(
        &self,
        metadata: &crate::search::EmbeddingMetadata,
    ) -> io::Result<Option<crate::search::VectorSidecar>> {
        let path = self.vector_sidecar_path(metadata)?;
        if !path.exists() {
            return Ok(None);
        }
        crate::search::VectorSidecar::load_from_path(&path).map(Some)
    }

    /// Compare a loaded vector sidecar with the current chain state.
    pub fn vector_sidecar_freshness(
        &self,
        sidecar: &crate::search::VectorSidecar,
        metadata: &crate::search::EmbeddingMetadata,
    ) -> io::Result<crate::search::VectorSidecarFreshness> {
        let Some(persistence) = &self.persistence else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this MentisDb handle does not expose stable persistence metadata",
            ));
        };

        Ok(sidecar.freshness(
            &persistence.chain_key,
            self.thoughts.len(),
            self.head_hash(),
            metadata,
        ))
    }

    /// Rebuild and persist the vector sidecar for one embedding provider.
    pub fn rebuild_vector_sidecar<P: crate::search::EmbeddingProvider>(
        &self,
        provider: &P,
    ) -> Result<crate::search::VectorSidecar, VectorSearchError<P::Error>> {
        let Some(persistence) = &self.persistence else {
            return Err(VectorSearchError::MissingPersistenceMetadata);
        };
        let path = chain_vector_sidecar_path(
            &persistence.chain_dir,
            &persistence.chain_key,
            persistence.storage_kind,
            provider.metadata(),
        );
        let inputs: Vec<crate::search::EmbeddingInput> = self
            .thoughts
            .iter()
            .map(|thought| {
                crate::search::EmbeddingInput::new(
                    thought.id.to_string(),
                    self.thought_embedding_text(thought),
                )
            })
            .collect();
        let documents = crate::search::embed_batch_to_documents(provider, &inputs)
            .map_err(VectorSearchError::Embedding)?;
        let entries: Vec<crate::search::VectorSidecarEntry> = self
            .thoughts
            .iter()
            .zip(documents)
            .map(|(thought, document)| {
                crate::search::VectorSidecarEntry::new(
                    thought.id,
                    thought.index,
                    thought.hash.clone(),
                    document.vector,
                )
            })
            .collect();
        let sidecar = crate::search::VectorSidecar::build(
            persistence.chain_key.clone(),
            provider.metadata().clone(),
            self.thoughts.len(),
            self.head_hash().map(ToOwned::to_owned),
            Utc::now(),
            entries,
        )
        .map_err(VectorSearchError::Io)?;
        sidecar.save_to_path(&path).map_err(VectorSearchError::Io)?;
        Ok(sidecar)
    }

    /// Rebuild one vector sidecar and keep it synchronized on future appends
    /// for this handle.
    ///
    /// This remains opt-in and handle-local: embeddings are still optional, and
    /// callers must re-register providers after reopening a chain.
    pub fn manage_vector_sidecar<P: crate::search::EmbeddingProvider + Send + Sync + 'static>(
        &mut self,
        provider: P,
    ) -> Result<crate::search::VectorSidecar, VectorSearchError<P::Error>> {
        let sidecar = match self
            .load_vector_sidecar(provider.metadata())
            .map_err(VectorSearchError::Io)?
        {
            Some(sidecar)
                if matches!(
                    self.vector_sidecar_freshness(&sidecar, provider.metadata())
                        .map_err(VectorSearchError::Io)?,
                    crate::search::VectorSidecarFreshness::Fresh
                ) =>
            {
                sidecar
            }
            _ => self.rebuild_vector_sidecar(&provider)?,
        };
        self.managed_vector_sidecars.insert(
            provider.metadata().clone(),
            Box::new(RegisteredEmbeddingProvider { provider }),
        );
        Ok(sidecar)
    }

    /// Stop append-time synchronization for one managed vector sidecar.
    pub fn unmanage_vector_sidecar(&mut self, metadata: &crate::search::EmbeddingMetadata) -> bool {
        self.managed_vector_sidecars.remove(metadata).is_some()
    }

    /// Return the embedding spaces currently managed for append-time vector
    /// sidecar synchronization on this handle.
    pub fn managed_vector_sidecars(&self) -> Vec<crate::search::EmbeddingMetadata> {
        let mut managed: Vec<_> = self.managed_vector_sidecars.keys().cloned().collect();
        managed.sort_by(|left, right| {
            left.model_id
                .cmp(&right.model_id)
                .then_with(|| left.embedding_version.cmp(&right.embedding_version))
                .then_with(|| left.dimension.cmp(&right.dimension))
        });
        managed
    }

    /// Load the persisted managed-vector settings for this chain and apply them
    /// to the current handle.
    ///
    /// `mentisdbd` uses this when it opens a chain so vector sidecars remain
    /// enabled across daemon restarts.
    pub fn apply_persisted_managed_vector_sidecars(&mut self) -> io::Result<()> {
        let config = self.persisted_managed_vector_sidecar_config()?;
        for provider in config.providers {
            let metadata = provider.provider_kind.metadata();
            if provider.enabled {
                self.manage_vector_sidecar(provider.provider_kind.provider())
                    .map_err(vector_search_error_to_io::<crate::search::LocalTextEmbeddingError>)?;
            } else {
                self.unmanage_vector_sidecar(&metadata);
            }
        }
        Ok(())
    }

    /// Return runtime status for every persistently configured managed vector
    /// sidecar on this chain.
    pub fn managed_vector_sidecar_statuses(&self) -> io::Result<Vec<ManagedVectorSidecarStatus>> {
        let mut statuses: Vec<_> = self
            .persisted_managed_vector_sidecar_config()?
            .providers
            .into_iter()
            .map(|config| self.managed_vector_sidecar_status(config.provider_kind, config.enabled))
            .collect::<io::Result<Vec<_>>>()?;
        statuses.sort_by(|left, right| left.provider_key.cmp(&right.provider_key));
        Ok(statuses)
    }

    /// Enable or disable append-time auto-sync for one managed vector provider
    /// on this chain and persist that setting.
    pub fn set_managed_vector_sidecar_enabled(
        &mut self,
        provider_kind: ManagedVectorProviderKind,
        enabled: bool,
    ) -> io::Result<ManagedVectorSidecarStatus> {
        let mut config = self.persisted_managed_vector_sidecar_config()?;
        let mut updated = false;
        for provider in &mut config.providers {
            if provider.provider_kind == provider_kind {
                provider.enabled = enabled;
                updated = true;
            }
        }
        if !updated {
            config.providers.push(ManagedVectorSidecarConfig {
                provider_kind,
                enabled,
            });
        }
        config.providers = normalize_managed_vector_sidecar_configs(config.providers);
        self.save_managed_vector_sidecar_config(&config)?;
        if enabled {
            self.manage_vector_sidecar(provider_kind.provider())
                .map_err(vector_search_error_to_io::<crate::search::LocalTextEmbeddingError>)?;
        } else {
            self.unmanage_vector_sidecar(&provider_kind.metadata());
        }
        self.managed_vector_sidecar_status(provider_kind, enabled)
    }

    /// Rebuild one managed vector sidecar to match the current chain state
    /// without changing whether append-time auto-sync is enabled.
    pub fn sync_managed_vector_sidecar_now(
        &mut self,
        provider_kind: ManagedVectorProviderKind,
    ) -> io::Result<ManagedVectorSidecarStatus> {
        let enabled = self
            .persisted_managed_vector_sidecar_config()?
            .providers
            .into_iter()
            .find(|provider| provider.provider_kind == provider_kind)
            .map(|provider| provider.enabled)
            .unwrap_or(true);
        let provider = provider_kind.provider();
        let metadata =
            <crate::search::LocalTextEmbeddingProvider as crate::search::EmbeddingProvider>::metadata(
                &provider,
            )
            .clone();
        self.rebuild_vector_sidecar(&provider)
            .map_err(vector_search_error_to_io::<crate::search::LocalTextEmbeddingError>)?;
        if enabled {
            self.managed_vector_sidecars
                .insert(metadata, Box::new(RegisteredEmbeddingProvider { provider }));
        } else {
            self.unmanage_vector_sidecar(&metadata);
        }
        self.managed_vector_sidecar_status(provider_kind, enabled)
    }

    /// Delete one managed vector sidecar file, if it exists, and rebuild it
    /// from the canonical chain log.
    pub fn rebuild_managed_vector_sidecar_from_scratch(
        &mut self,
        provider_kind: ManagedVectorProviderKind,
    ) -> io::Result<ManagedVectorSidecarStatus> {
        let provider = provider_kind.provider();
        let path = self.vector_sidecar_path(
            <crate::search::LocalTextEmbeddingProvider as crate::search::EmbeddingProvider>::metadata(
                &provider,
            ),
        )?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        self.sync_managed_vector_sidecar_now(provider_kind)
    }

    /// Query a persisted vector sidecar with provider-generated query embeddings.
    ///
    /// This does not change default retrieval behavior. Callers must rebuild the
    /// sidecar explicitly via [`MentisDb::rebuild_vector_sidecar`] before vector
    /// search becomes available for a chain and embedding space.
    pub fn query_vector<P: crate::search::EmbeddingProvider>(
        &self,
        provider: &P,
        request: &VectorSearchQuery,
    ) -> Result<VectorSearchResult<'_>, VectorSearchError<P::Error>> {
        let metadata = provider.metadata().clone();
        let sidecar_path =
            self.vector_sidecar_path(&metadata)
                .map_err(|error| match error.kind() {
                    io::ErrorKind::Unsupported => VectorSearchError::MissingPersistenceMetadata,
                    _ => VectorSearchError::Io(error),
                })?;
        let sidecar = self
            .load_vector_sidecar(&metadata)
            .map_err(VectorSearchError::Io)?
            .ok_or_else(|| VectorSearchError::MissingSidecar(sidecar_path.clone()))?;
        let freshness = self
            .vector_sidecar_freshness(&sidecar, &metadata)
            .map_err(VectorSearchError::Io)?;
        let query_text = request.text.trim();
        if query_text.is_empty() {
            return Ok(VectorSearchResult {
                metadata,
                freshness,
                total_candidates: 0,
                hits: Vec::new(),
            });
        }

        let mut embedded_query = crate::search::embed_batch_to_documents(
            provider,
            &[crate::search::EmbeddingInput::new("__query__", query_text)],
        )
        .map_err(VectorSearchError::Embedding)?;
        let query_vector = embedded_query
            .pop()
            .map(|document| document.vector)
            .unwrap_or_default();

        let candidate_ids: HashSet<Uuid> = self
            .query(&request.filter)
            .into_iter()
            .map(|thought| thought.id)
            .collect();
        let documents: Vec<crate::search::VectorDocument> = sidecar
            .entries
            .iter()
            .filter(|entry| candidate_ids.contains(&entry.thought_id))
            .map(|entry| {
                crate::search::VectorDocument::new(
                    entry.thought_id.to_string(),
                    entry.vector.clone(),
                )
            })
            .collect();
        let total_candidates = documents.len();
        if documents.is_empty() {
            return Ok(VectorSearchResult {
                metadata,
                freshness,
                total_candidates,
                hits: Vec::new(),
            });
        }

        let index = crate::search::VectorIndex::from_documents(metadata.clone(), documents)
            .map_err(VectorSearchError::Index)?;
        let mut hits = index
            .search(&crate::search::VectorQuery::new(query_vector).with_limit(request.limit))
            .map_err(VectorSearchError::Index)?;
        if let Some(min_score) = request.min_score {
            hits.retain(|hit| hit.score >= min_score);
        }

        let hits = hits
            .into_iter()
            .filter_map(|hit| {
                let thought_id = Uuid::parse_str(&hit.document_id).ok()?;
                let position = *self.id_to_index.get(&thought_id)?;
                Some(VectorSearchHit {
                    thought: &self.thoughts[position],
                    score: hit.score,
                    freshness: freshness.clone(),
                })
            })
            .collect();

        Ok(VectorSearchResult {
            metadata,
            freshness,
            total_candidates,
            hits,
        })
    }

    fn sync_managed_vector_sidecars_for_append(&self, thought: &Thought) -> io::Result<()> {
        let Some(persistence) = &self.persistence else {
            return Ok(());
        };
        if self.managed_vector_sidecars.is_empty() {
            return Ok(());
        }

        let previous_thought_count = self.thoughts.len().saturating_sub(1);
        let previous_head_hash = if thought.prev_hash.is_empty() {
            None
        } else {
            Some(thought.prev_hash.as_str())
        };

        for provider in self.managed_vector_sidecars.values() {
            let metadata = provider.metadata().clone();
            let path = chain_vector_sidecar_path(
                &persistence.chain_dir,
                &persistence.chain_key,
                persistence.storage_kind,
                &metadata,
            );
            let sidecar = match crate::search::VectorSidecar::load_from_path(&path) {
                Ok(sidecar)
                    if matches!(
                        sidecar.freshness(
                            &persistence.chain_key,
                            previous_thought_count,
                            previous_head_hash,
                            &metadata,
                        ),
                        crate::search::VectorSidecarFreshness::Fresh
                    ) =>
                {
                    self.extend_fresh_vector_sidecar(provider.as_ref(), sidecar, thought)?
                }
                Ok(_) | Err(_) => self.rebuild_managed_vector_sidecar(provider.as_ref())?,
            };
            sidecar.save_to_path(&path)?;
        }
        Ok(())
    }

    fn extend_fresh_vector_sidecar(
        &self,
        provider: &dyn ManagedEmbeddingProvider,
        sidecar: crate::search::VectorSidecar,
        thought: &Thought,
    ) -> io::Result<crate::search::VectorSidecar> {
        let mut documents = provider.embed_documents(&[crate::search::EmbeddingInput::new(
            thought.id.to_string(),
            self.thought_embedding_text(thought),
        )])?;
        let vector = documents
            .pop()
            .map(|document| document.vector)
            .ok_or_else(|| io::Error::other("managed embedding provider returned no vectors"))?;
        let mut entries = sidecar.entries;
        entries.push(crate::search::VectorSidecarEntry::new(
            thought.id,
            thought.index,
            thought.hash.clone(),
            vector,
        ));
        crate::search::VectorSidecar::build(
            sidecar.chain_key,
            sidecar.metadata,
            self.thoughts.len(),
            self.head_hash().map(ToOwned::to_owned),
            Utc::now(),
            entries,
        )
    }

    fn rebuild_managed_vector_sidecar(
        &self,
        provider: &dyn ManagedEmbeddingProvider,
    ) -> io::Result<crate::search::VectorSidecar> {
        let Some(persistence) = &self.persistence else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this MentisDb handle does not expose stable persistence metadata",
            ));
        };

        let inputs: Vec<crate::search::EmbeddingInput> = self
            .thoughts
            .iter()
            .map(|thought| {
                crate::search::EmbeddingInput::new(
                    thought.id.to_string(),
                    self.thought_embedding_text(thought),
                )
            })
            .collect();
        let documents = provider.embed_documents(&inputs)?;
        let entries: Vec<crate::search::VectorSidecarEntry> = self
            .thoughts
            .iter()
            .zip(documents)
            .map(|(thought, document)| {
                crate::search::VectorSidecarEntry::new(
                    thought.id,
                    thought.index,
                    thought.hash.clone(),
                    document.vector,
                )
            })
            .collect();
        crate::search::VectorSidecar::build(
            persistence.chain_key.clone(),
            provider.metadata().clone(),
            self.thoughts.len(),
            self.head_hash().map(ToOwned::to_owned),
            Utc::now(),
            entries,
        )
    }

    fn persisted_managed_vector_sidecar_config(
        &self,
    ) -> io::Result<ManagedVectorSidecarConfigFile> {
        let persistence = self.persistence_metadata()?;
        let config = load_managed_vector_sidecar_config(
            &persistence.chain_dir,
            &persistence.chain_key,
            persistence.storage_kind,
        )?;
        save_managed_vector_sidecar_config(
            &persistence.chain_dir,
            &persistence.chain_key,
            persistence.storage_kind,
            &config,
        )?;
        Ok(config)
    }

    fn save_managed_vector_sidecar_config(
        &self,
        config: &ManagedVectorSidecarConfigFile,
    ) -> io::Result<()> {
        let persistence = self.persistence_metadata()?;
        save_managed_vector_sidecar_config(
            &persistence.chain_dir,
            &persistence.chain_key,
            persistence.storage_kind,
            config,
        )
    }

    fn persistence_metadata(&self) -> io::Result<&ChainPersistenceMetadata> {
        self.persistence.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "this MentisDb handle does not expose stable persistence metadata",
            )
        })
    }

    fn managed_vector_sidecar_status(
        &self,
        provider_kind: ManagedVectorProviderKind,
        enabled: bool,
    ) -> io::Result<ManagedVectorSidecarStatus> {
        let metadata = provider_kind.metadata();
        let sidecar_path = self.vector_sidecar_path(&metadata)?;
        let sidecar_exists = sidecar_path.exists();
        let mut freshness = None;
        let mut load_error = None;
        let mut indexed_thought_count = None;
        let mut generated_at = None;

        match self.load_vector_sidecar(&metadata) {
            Ok(Some(sidecar)) => {
                freshness = Some(self.vector_sidecar_freshness(&sidecar, &metadata)?);
                indexed_thought_count = Some(sidecar.entries.len());
                generated_at = Some(sidecar.generated_at);
            }
            Ok(None) => {}
            Err(error) => {
                load_error = Some(error.to_string());
            }
        }

        Ok(ManagedVectorSidecarStatus {
            provider_kind,
            provider_key: provider_kind.key().to_string(),
            enabled,
            managed_in_memory: self.managed_vector_sidecars.contains_key(&metadata),
            metadata,
            sidecar_path: sidecar_path.display().to_string(),
            sidecar_exists,
            freshness,
            load_error,
            thought_count: self.thoughts.len(),
            indexed_thought_count,
            generated_at,
        })
    }

    fn rank_search_hit<'a>(
        &'a self,
        thought: &'a Thought,
        ranked_text: Option<&str>,
        lexical_hits: &HashMap<usize, crate::search::lexical::LexicalHit>,
        vector_scores: &HashMap<usize, f32>,
        graph_hits: &HashMap<usize, RankedGraphHit>,
    ) -> Option<RankedSearchHit<'a>> {
        let (
            lexical,
            vector,
            graph,
            relation,
            seed_support,
            graph_distance,
            graph_seed_paths,
            graph_relation_kinds,
            graph_path,
            matched_terms,
            match_sources,
        ) = if ranked_text.is_some() {
            let lexical_hit = lexical_hits
                .get(&(thought.index as usize))
                .filter(|hit| hit.score > 0.0);
            let graph_hit = graph_hits.get(&(thought.index as usize));
            let vector = vector_scores
                .get(&(thought.index as usize))
                .copied()
                .unwrap_or_default();

            if lexical_hit.is_none() && graph_hit.is_none() && vector <= 0.0 {
                return None;
            }

            (
                lexical_hit.map(|hit| hit.score).unwrap_or_default(),
                vector,
                graph_hit
                    .map(|hit| self.graph_proximity_score(hit.best_hit.depth))
                    .unwrap_or_default(),
                graph_hit.map(|hit| hit.relation_score).unwrap_or_default(),
                graph_hit
                    .map(|hit| self.graph_seed_support_score(hit.seed_paths))
                    .unwrap_or_default(),
                graph_hit.map(|hit| hit.best_hit.depth),
                graph_hit.map(|hit| hit.seed_paths).unwrap_or(0),
                graph_hit
                    .map(|hit| hit.relation_kinds.clone())
                    .unwrap_or_default(),
                graph_hit.map(|hit| hit.best_hit.path.clone()),
                lexical_hit
                    .map(|hit| hit.matched_terms.clone())
                    .unwrap_or_default(),
                lexical_hit
                    .map(|hit| hit.match_sources.clone())
                    .unwrap_or_default(),
            )
        } else {
            (
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                None,
                0,
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
            )
        };
        let importance = thought.importance * 0.2;
        let confidence = thought.confidence.unwrap_or_default() * 0.1;
        let recency = self.recency_score(thought);
        let total =
            lexical + vector + graph + relation + seed_support + importance + confidence + recency;

        Some(RankedSearchHit {
            thought,
            score: RankedSearchScore {
                lexical,
                vector,
                graph,
                relation,
                seed_support,
                importance,
                confidence,
                recency,
                total,
            },
            graph_distance,
            graph_seed_paths,
            graph_relation_kinds,
            graph_path,
            matched_terms,
            match_sources,
        })
    }

    fn rank_candidates_lexically(
        &self,
        candidates: &[&Thought],
        text: &str,
    ) -> HashMap<usize, crate::search::lexical::LexicalHit> {
        let positions: Vec<usize> = candidates
            .iter()
            .map(|thought| thought.index as usize)
            .collect();
        let index = crate::search::lexical::LexicalIndex::build_with_registry(
            &self.thoughts,
            self.agent_registry(),
        );
        index
            .search_in_positions(&crate::search::lexical::LexicalQuery::new(text), &positions)
            .into_iter()
            .map(|hit| (hit.doc_position, hit))
            .collect()
    }

    fn rank_candidates_semantically(
        &self,
        candidates: &[&Thought],
        text: &str,
    ) -> HashMap<usize, f32> {
        const FRESH_VECTOR_WEIGHT: f32 = 0.35;
        const STALE_VECTOR_WEIGHT: f32 = 0.2;
        const MIN_VECTOR_COSINE: f32 = 0.12;
        const MAX_VECTOR_HITS: usize = 256;

        if candidates.is_empty() || self.managed_vector_sidecars.is_empty() {
            return HashMap::new();
        }

        let candidate_positions: HashMap<String, usize> = candidates
            .iter()
            .map(|thought| (thought.id.to_string(), thought.index as usize))
            .collect();
        let mut scores = HashMap::new();

        for provider in self.managed_vector_sidecars.values() {
            let metadata = provider.metadata().clone();
            let sidecar = match self.load_vector_sidecar(&metadata) {
                Ok(Some(sidecar)) => sidecar,
                Ok(None) | Err(_) => continue,
            };
            let freshness_weight = match self.vector_sidecar_freshness(&sidecar, &metadata) {
                Ok(crate::search::VectorSidecarFreshness::Fresh) => FRESH_VECTOR_WEIGHT,
                Ok(
                    crate::search::VectorSidecarFreshness::StaleThoughtCount { .. }
                    | crate::search::VectorSidecarFreshness::StaleHeadHash { .. },
                ) => STALE_VECTOR_WEIGHT,
                Ok(_) | Err(_) => continue,
            };

            let mut query_documents = match provider
                .embed_documents(&[crate::search::EmbeddingInput::new("__query__", text)])
            {
                Ok(documents) => documents,
                Err(_) => continue,
            };
            let Some(query_vector) = query_documents.pop().map(|document| document.vector) else {
                continue;
            };

            let documents: Vec<crate::search::VectorDocument> = sidecar
                .entries
                .iter()
                .filter(|entry| candidate_positions.contains_key(&entry.thought_id.to_string()))
                .map(|entry| {
                    crate::search::VectorDocument::new(
                        entry.thought_id.to_string(),
                        entry.vector.clone(),
                    )
                })
                .collect();
            if documents.is_empty() {
                continue;
            }

            let limit = documents.len().clamp(1, MAX_VECTOR_HITS);
            let index = match crate::search::VectorIndex::from_documents(metadata, documents) {
                Ok(index) => index,
                Err(_) => continue,
            };
            let hits = match index
                .search(&crate::search::VectorQuery::new(query_vector).with_limit(limit))
            {
                Ok(hits) => hits,
                Err(_) => continue,
            };

            for hit in hits {
                if hit.score < MIN_VECTOR_COSINE {
                    break;
                }
                if let Some(position) = candidate_positions.get(&hit.document_id) {
                    let weighted_score = hit.score * freshness_weight;
                    scores
                        .entry(*position)
                        .and_modify(|existing: &mut f32| *existing = existing.max(weighted_score))
                        .or_insert(weighted_score);
                }
            }
        }

        scores
    }

    fn context_bundle_seeds(
        &self,
        lexical_hits: &HashMap<usize, crate::search::lexical::LexicalHit>,
        limit: usize,
    ) -> Vec<crate::search::ContextBundleSeed> {
        self.sorted_lexical_seed_hits(lexical_hits)
            .into_iter()
            .take(limit.max(1))
            .filter_map(|hit| {
                self.thoughts
                    .get(hit.doc_position)
                    .map(crate::search::ThoughtLocator::local)
                    .map(|locator| {
                        crate::search::ContextBundleSeed::new(locator, hit.score)
                            .with_matched_terms(hit.matched_terms.iter().cloned())
                    })
            })
            .collect()
    }

    fn expand_ranked_candidates(
        &self,
        candidates: &[&Thought],
        graph: &RankedSearchGraph,
        lexical_hits: &HashMap<usize, crate::search::lexical::LexicalHit>,
    ) -> HashMap<usize, RankedGraphHit> {
        let raw_hits = self.expand_ranked_candidate_paths(candidates, graph, lexical_hits);
        let mut aggregates = HashMap::<usize, RankedGraphHit>::new();

        for hit in raw_hits {
            let Some(position) = hit.locator.thought_index.map(|index| index as usize) else {
                continue;
            };
            let relation_kinds = self.graph_path_relation_kinds(&hit.path);
            let relation_score = self.graph_relation_score(&relation_kinds, hit.depth);

            match aggregates.entry(position) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(RankedGraphHit {
                        best_hit: hit,
                        seed_paths: 1,
                        relation_kinds,
                        relation_score,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let aggregate = entry.get_mut();
                    aggregate.seed_paths += 1;
                    merge_relation_kinds(&mut aggregate.relation_kinds, &relation_kinds);
                    if self.is_better_graph_hit(
                        &hit,
                        relation_score,
                        &aggregate.best_hit,
                        aggregate.relation_score,
                    ) {
                        aggregate.best_hit = hit;
                        aggregate.relation_score = relation_score;
                    }
                }
            }
        }

        aggregates
    }

    fn expand_ranked_candidate_paths(
        &self,
        candidates: &[&Thought],
        graph: &RankedSearchGraph,
        lexical_hits: &HashMap<usize, crate::search::lexical::LexicalHit>,
    ) -> Vec<crate::search::GraphExpansionHit> {
        if lexical_hits.is_empty() {
            return Vec::new();
        }

        let adjacency = crate::search::ThoughtAdjacencyIndex::from_thoughts(&self.thoughts);
        let candidate_positions: HashSet<usize> = candidates
            .iter()
            .map(|thought| thought.index as usize)
            .collect();
        let seeds: Vec<crate::search::ThoughtLocator> = self
            .sorted_lexical_seed_hits(lexical_hits)
            .into_iter()
            .filter_map(|hit| {
                adjacency
                    .local_locator_for_index(hit.doc_position as u64)
                    .cloned()
            })
            .collect();
        if seeds.is_empty() {
            return Vec::new();
        }
        let seed_positions: HashSet<usize> = seeds
            .iter()
            .filter_map(|seed| seed.thought_index.map(|index| index as usize))
            .collect();

        let mut hits = Vec::new();
        for seed in seeds {
            let seed_position = seed.thought_index.map(|index| index as usize);
            let seed_result = crate::search::GraphExpansionResult::expand(
                &adjacency,
                &crate::search::GraphExpansionQuery::new(vec![seed])
                    .with_max_depth(graph.max_depth)
                    .with_max_visited(graph.max_visited)
                    .with_include_seeds(graph.include_seeds)
                    .with_mode(graph.mode),
            );

            hits.extend(seed_result.hits.into_iter().filter(|hit| {
                hit.locator
                    .thought_index
                    .map(|index| {
                        let position = index as usize;
                        candidate_positions.contains(&position)
                            && (!seed_positions.contains(&position)
                                || Some(position) == seed_position)
                    })
                    .unwrap_or(false)
            }));
        }

        hits
    }

    fn sorted_lexical_seed_hits<'a>(
        &self,
        lexical_hits: &'a HashMap<usize, crate::search::lexical::LexicalHit>,
    ) -> Vec<&'a crate::search::lexical::LexicalHit> {
        let mut lexical_seed_hits: Vec<&crate::search::lexical::LexicalHit> =
            lexical_hits.values().collect();
        lexical_seed_hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.doc_position.cmp(&right.doc_position))
        });
        lexical_seed_hits
    }

    fn graph_proximity_score(&self, depth: usize) -> f32 {
        if depth == 0 {
            0.0
        } else {
            0.3 / depth as f32
        }
    }

    fn graph_seed_support_score(&self, seed_paths: usize) -> f32 {
        seed_paths.saturating_sub(1).min(3) as f32 * 0.05
    }

    fn graph_path_relation_kinds(
        &self,
        path: &crate::search::GraphExpansionPath,
    ) -> Vec<ThoughtRelationKind> {
        let mut kinds = Vec::new();
        for hop in &path.hops {
            for provenance in &hop.edge.provenances {
                if let crate::search::GraphEdgeProvenance::Relation { kind, .. } = provenance {
                    if !kinds.contains(kind) {
                        kinds.push(*kind);
                    }
                }
            }
        }
        kinds
    }

    fn graph_relation_score(&self, relation_kinds: &[ThoughtRelationKind], depth: usize) -> f32 {
        if relation_kinds.is_empty() {
            return 0.0;
        }
        let strongest = relation_kinds
            .iter()
            .map(|kind| self.graph_relation_kind_boost(*kind))
            .fold(0.0_f32, f32::max);
        strongest / depth.max(1) as f32
    }

    fn graph_relation_kind_boost(&self, kind: ThoughtRelationKind) -> f32 {
        match kind {
            ThoughtRelationKind::Corrects => 0.18,
            ThoughtRelationKind::Invalidates => 0.18,
            ThoughtRelationKind::Supersedes => 0.16,
            ThoughtRelationKind::DerivedFrom => 0.14,
            ThoughtRelationKind::ContinuesFrom => 0.12,
            ThoughtRelationKind::Summarizes => 0.11,
            ThoughtRelationKind::CausedBy => 0.11,
            ThoughtRelationKind::Supports => 0.09,
            ThoughtRelationKind::Contradicts => 0.09,
            ThoughtRelationKind::RelatedTo => 0.05,
            ThoughtRelationKind::References => 0.04,
        }
    }

    fn is_better_graph_hit(
        &self,
        candidate: &crate::search::GraphExpansionHit,
        candidate_relation_score: f32,
        current: &crate::search::GraphExpansionHit,
        current_relation_score: f32,
    ) -> bool {
        candidate.depth < current.depth
            || (candidate.depth == current.depth
                && (candidate_relation_score > current_relation_score
                    || (candidate_relation_score == current_relation_score
                        && candidate.locator < current.locator)))
    }

    fn recency_score(&self, thought: &Thought) -> f32 {
        let newest_index = self.thoughts.len().saturating_sub(1);
        if newest_index == 0 {
            return 0.05;
        }

        (thought.index as f32 / newest_index as f32) * 0.05
    }

    fn thought_embedding_text(&self, thought: &Thought) -> String {
        let mut sections = Vec::new();
        let content = thought.content.trim();
        if !content.is_empty() {
            sections.push(content.to_string());
        }
        if !thought.concepts.is_empty() {
            sections.push(format!("Concepts: {}", thought.concepts.join(", ")));
        }
        if !thought.tags.is_empty() {
            sections.push(format!("Tags: {}", thought.tags.join(", ")));
        }
        if let Some(record) = self.agent_record_for(&thought.agent_id) {
            if !record.display_name.trim().is_empty() && record.display_name != thought.agent_id {
                sections.push(format!("Agent: {}", record.display_name));
            }
            if !record.aliases.is_empty() {
                sections.push(format!("Aliases: {}", record.aliases.join(", ")));
            }
            if let Some(owner) = record
                .owner
                .as_ref()
                .filter(|owner| !owner.trim().is_empty())
            {
                sections.push(format!("Owner: {owner}"));
            }
            if let Some(description) = record
                .description
                .as_ref()
                .filter(|description| !description.trim().is_empty())
            {
                sections.push(format!("Description: {description}"));
            }
        }
        sections.join("\n")
    }

    fn indexed_candidate_positions(&self, query: &ThoughtQuery) -> Option<Vec<usize>> {
        let mut filters = Vec::new();

        if let Some(thought_types) = &query.thought_types {
            filters.push(union_position_lists(thought_types.iter().filter_map(
                |thought_type| self.query_indexes.by_thought_type.get(thought_type),
            )));
        }

        if let Some(roles) = &query.roles {
            filters.push(union_position_lists(
                roles
                    .iter()
                    .filter_map(|role| self.query_indexes.by_role.get(role)),
            ));
        }

        if let Some(agent_ids) = &query.agent_ids {
            filters.push(union_position_lists(
                agent_ids
                    .iter()
                    .filter_map(|agent_id| self.query_indexes.by_agent_id.get(agent_id)),
            ));
        }

        if !query.tags_any.is_empty() {
            filters.push(matching_index_positions(
                &self.query_indexes.by_tag,
                &query.tags_any,
            ));
        }

        if !query.concepts_any.is_empty() {
            filters.push(matching_index_positions(
                &self.query_indexes.by_concept,
                &query.concepts_any,
            ));
        }

        if let Some(agent_names) = &query.agent_names {
            let matching_agent_ids: Vec<&str> = self
                .agent_registry
                .agents
                .values()
                .filter(|record| {
                    agent_names.iter().any(|agent_name| {
                        equals_case_insensitive(&record.display_name, agent_name)
                            || record
                                .aliases
                                .iter()
                                .any(|alias| equals_case_insensitive(alias, agent_name))
                    })
                })
                .map(|record| record.agent_id.as_str())
                .collect();
            filters.push(union_position_lists(
                matching_agent_ids
                    .into_iter()
                    .filter_map(|agent_id| self.query_indexes.by_agent_id.get(agent_id)),
            ));
        }

        if let Some(agent_owners) = &query.agent_owners {
            let matching_agent_ids: Vec<&str> = self
                .agent_registry
                .agents
                .values()
                .filter(|record| {
                    record.owner.as_ref().is_some_and(|owner| {
                        agent_owners
                            .iter()
                            .any(|agent_owner| equals_case_insensitive(owner, agent_owner))
                    })
                })
                .map(|record| record.agent_id.as_str())
                .collect();
            filters.push(union_position_lists(
                matching_agent_ids
                    .into_iter()
                    .filter_map(|agent_id| self.query_indexes.by_agent_id.get(agent_id)),
            ));
        }

        let mut filters = filters.into_iter();
        let first = filters.next()?;
        Some(filters.fold(first, |acc, positions| {
            intersect_sorted_positions(&acc, &positions)
        }))
    }

    /// Convenience helper to find thoughts related to a concept string.
    pub fn related_to_concept(&self, concept: &str, limit: usize) -> Vec<&Thought> {
        self.query(
            &ThoughtQuery::new()
                .with_concepts_any([concept])
                .with_limit(limit),
        )
    }

    /// Render a context reconstruction prompt for a target thought.
    pub fn to_bootstrap_prompt(&self, target_index: u64) -> String {
        let resolved = self.resolve_context(target_index);
        if resolved.is_empty() {
            return String::new();
        }

        let mut prompt = String::from("=== RESTORED CONTEXT (from MentisDb) ===\n\n");
        for thought in resolved {
            prompt.push_str(&format!(
                "[#{}] {:?} / {:?} ({})\n{}\n",
                thought.index,
                thought.thought_type,
                thought.role,
                self.agent_label_for(thought),
                thought.content
            ));
            if let Some(confidence) = thought.confidence {
                prompt.push_str(&format!("  confidence: {:.2}\n", confidence));
            }
            prompt.push_str(&format!("  importance: {:.2}\n", thought.importance));
            if !thought.tags.is_empty() {
                prompt.push_str(&format!("  tags: {}\n", thought.tags.join(", ")));
            }
            if !thought.concepts.is_empty() {
                prompt.push_str(&format!("  concepts: {}\n", thought.concepts.join(", ")));
            }
            if !thought.refs.is_empty() {
                prompt.push_str(&format!("  refs: {:?}\n", thought.refs));
            }
        }
        prompt.push_str("\n=== END RESTORED CONTEXT ===\n");
        prompt
    }

    /// Render the last `n` thoughts as a lightweight catch-up prompt.
    pub fn to_catchup_prompt(&self, last_n: usize) -> String {
        let start = self.thoughts.len().saturating_sub(last_n);
        let tail = &self.thoughts[start..];
        if tail.is_empty() {
            return String::new();
        }

        let mut prompt = String::from("=== RECENT CONTEXT ===\n\n");
        for thought in tail {
            prompt.push_str(&format!(
                "[#{}] {:?} / {:?} ({}) {}\n",
                thought.index,
                thought.thought_type,
                thought.role,
                self.agent_label_for(thought),
                thought.content
            ));
        }
        prompt.push_str("\n=== END RECENT CONTEXT ===\n");
        prompt
    }

    /// Export a Markdown memory view.
    ///
    /// This is suitable for generating a `MEMORY.md`-style summary from a full
    /// chain or a queried subset of thoughts.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::{MentisDb, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let mut chain = MentisDb::open(&PathBuf::from("/tmp/tc_md"), "agent1", "Agent", None, None)?;
    /// chain.append("agent1", ThoughtType::PreferenceUpdate, "User prefers concise Markdown.")?;
    ///
    /// let markdown = chain.to_memory_markdown(None);
    /// assert!(markdown.contains("# MEMORY"));
    /// # Ok(())
    /// # }
    /// ```
    pub fn to_memory_markdown(&self, query: Option<&ThoughtQuery>) -> String {
        let thoughts = query
            .map(|query| self.query(query))
            .unwrap_or_else(|| self.thoughts.iter().collect());

        let mut markdown = String::from("# MEMORY\n\n");
        markdown.push_str(&format!(
            "Generated from `{}` with {} thought(s).\n\n",
            self.storage.storage_location(),
            thoughts.len()
        ));

        append_memory_section(
            &mut markdown,
            self,
            "Identity",
            &thoughts,
            &[
                ThoughtType::PreferenceUpdate,
                ThoughtType::UserTrait,
                ThoughtType::RelationshipUpdate,
            ],
        );
        append_memory_section(
            &mut markdown,
            self,
            "Knowledge",
            &thoughts,
            &[
                ThoughtType::Finding,
                ThoughtType::Insight,
                ThoughtType::FactLearned,
                ThoughtType::PatternDetected,
                ThoughtType::Hypothesis,
                ThoughtType::Surprise,
            ],
        );
        append_memory_section(
            &mut markdown,
            self,
            "Constraints And Decisions",
            &thoughts,
            &[
                ThoughtType::Constraint,
                ThoughtType::Plan,
                ThoughtType::Subgoal,
                ThoughtType::Decision,
                ThoughtType::StrategyShift,
            ],
        );
        append_memory_section(
            &mut markdown,
            self,
            "Corrections",
            &thoughts,
            &[
                ThoughtType::Mistake,
                ThoughtType::Correction,
                ThoughtType::LessonLearned,
                ThoughtType::AssumptionInvalidated,
                ThoughtType::Reframe,
            ],
        );
        append_memory_section(
            &mut markdown,
            self,
            "Open Threads",
            &thoughts,
            &[
                ThoughtType::Wonder,
                ThoughtType::Question,
                ThoughtType::Idea,
                ThoughtType::Experiment,
            ],
        );
        append_memory_section(
            &mut markdown,
            self,
            "Execution State",
            &thoughts,
            &[
                ThoughtType::ActionTaken,
                ThoughtType::TaskComplete,
                ThoughtType::Checkpoint,
                ThoughtType::StateSnapshot,
                ThoughtType::Handoff,
                ThoughtType::Summary,
            ],
        );

        markdown
    }

    /// Import thoughts from a MEMORY.md-formatted markdown string.
    ///
    /// Each line matching the format produced by [`MentisDb::to_memory_markdown`] is
    /// parsed and appended as a new thought:
    ///
    /// ```text
    /// - [#N] TypeName: content (agent agent_id; role RoleName; importance 0.85; confidence 0.90; tags tag1, tag2)
    /// ```
    ///
    /// The source index `[#N]` is **discarded** — thoughts receive new
    /// append-order indices assigned by this chain. Lines that do not match
    /// the pattern (section headers, blank lines, malformed entries) are
    /// silently skipped.
    ///
    /// # Parameters
    ///
    /// - `markdown` — MEMORY.md formatted content to import.
    /// - `default_agent_id` — Agent ID to use when a parsed line contains no
    ///   `agent` token in its metadata.
    ///
    /// # Returns
    ///
    /// The append-order indices of all successfully imported thoughts, in
    /// import order.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if any individual [`MentisDb::append_thought`] call fails.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// use std::path::PathBuf;
    /// use mentisdb::MentisDb;
    ///
    /// let mut chain = MentisDb::open_with_key(&PathBuf::from("/tmp/import_demo"), "demo").unwrap();
    /// let markdown = "## Decisions\n\
    ///     - [#0] Decision: Use PostgreSQL (agent alice; importance 0.90)\n";
    /// let indices = chain.import_from_memory_markdown(markdown, "default-agent").unwrap();
    /// assert_eq!(indices.len(), 1);
    /// ```
    pub fn import_from_memory_markdown(
        &mut self,
        markdown: &str,
        default_agent_id: &str,
    ) -> io::Result<Vec<u64>> {
        let mut indices = Vec::new();
        for line in markdown.lines() {
            let Some(parsed) = parse_memory_markdown_line(line) else {
                continue;
            };
            let agent_id = parsed.agent_id.as_deref().unwrap_or(default_agent_id);
            let mut input = ThoughtInput::new(parsed.thought_type, parsed.content)
                .with_role(parsed.role)
                .with_importance(parsed.importance)
                .with_tags(parsed.tags);
            if let Some(conf) = parsed.confidence {
                input = input.with_confidence(conf);
            }
            let thought = self.append_thought(agent_id, input)?;
            indices.push(thought.index);
        }
        Ok(indices)
    }

    /// Return all thoughts in chronological order.
    pub fn thoughts(&self) -> &[Thought] {
        &self.thoughts
    }

    /// Return the current head hash of the chain, if any.
    pub fn head_hash(&self) -> Option<&str> {
        self.head_thought().map(|thought| thought.hash.as_str())
    }

    /// Return a human-readable description of the underlying storage location.
    pub fn storage_location(&self) -> String {
        self.storage.storage_location()
    }

    /// Enable or disable immediate persistence on append.
    ///
    /// This also reconfigures the underlying storage adapter when the backend
    /// supports buffered writes.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the underlying storage adapter cannot switch
    /// modes or flush pending buffered writes during the transition.
    pub fn set_auto_flush(&mut self, auto_flush: bool) -> io::Result<()> {
        self.storage.set_auto_flush(auto_flush)?;
        self.auto_flush = auto_flush;
        if auto_flush {
            self.maybe_flush_agent_registry(true)?;
            self.maybe_flush_chain_registration(true)?;
        }
        Ok(())
    }

    /// Detach this chain from on-disk registry synchronization.
    ///
    /// This is used when a chain is being deleted while live handles still
    /// exist in memory. Without detaching persistence metadata first, the
    /// final live handle could re-register the chain during [`Drop`].
    pub fn detach_persistence(&mut self) {
        self.persistence = None;
        self.pending_agent_registry_sync = false;
        self.pending_agent_registry_updates = 0;
        self.pending_chain_registration_sync = false;
        self.pending_chain_registration_updates = 0;
    }

    fn persist_agent_registry(&self) -> io::Result<()> {
        if let Some(metadata) = &self.persistence {
            save_agent_registry(
                &metadata.chain_dir,
                &metadata.chain_key,
                metadata.storage_kind,
                &self.agent_registry,
            )?;
        }
        Ok(())
    }

    fn mark_agent_registry_dirty(&mut self) {
        if self.persistence.is_some() {
            self.pending_agent_registry_sync = true;
            self.pending_agent_registry_updates =
                self.pending_agent_registry_updates.saturating_add(1);
        }
    }

    fn maybe_flush_agent_registry(&mut self, force: bool) -> io::Result<()> {
        if !self.pending_agent_registry_sync {
            return Ok(());
        }
        if force
            || self.auto_flush
            || self.pending_agent_registry_updates >= AGENT_REGISTRY_FLUSH_THRESHOLD
        {
            self.persist_agent_registry()?;
            self.pending_agent_registry_sync = false;
            self.pending_agent_registry_updates = 0;
        }
        Ok(())
    }

    fn mark_chain_registration_dirty(&mut self) {
        if self.persistence.is_some() {
            self.pending_chain_registration_sync = true;
            self.pending_chain_registration_updates =
                self.pending_chain_registration_updates.saturating_add(1);
        }
    }

    fn maybe_flush_chain_registration(&mut self, force: bool) -> io::Result<()> {
        if !self.pending_chain_registration_sync {
            return Ok(());
        }
        if force || self.pending_chain_registration_updates >= CHAIN_REGISTRATION_FLUSH_THRESHOLD {
            self.persist_chain_registration()?;
            self.pending_chain_registration_sync = false;
            self.pending_chain_registration_updates = 0;
        }
        Ok(())
    }

    fn persist_registries(&mut self) -> io::Result<()> {
        self.mark_agent_registry_dirty();
        self.maybe_flush_agent_registry(true)?;
        self.mark_chain_registration_dirty();
        self.maybe_flush_chain_registration(true)
    }

    fn persist_chain_registration(&self) -> io::Result<()> {
        let Some(metadata) = &self.persistence else {
            return Ok(());
        };

        let mut registry = load_mentisdb_registry(&metadata.chain_dir)?;
        let now = Utc::now();
        let created_at = registry
            .chains
            .get(&metadata.chain_key)
            .map(|entry| entry.created_at)
            .unwrap_or(now);
        registry.chains.insert(
            metadata.chain_key.clone(),
            MentisDbRegistration {
                chain_key: metadata.chain_key.clone(),
                version: MENTISDB_CURRENT_VERSION,
                storage_adapter: metadata.storage_kind,
                storage_location: self.storage.storage_location(),
                thought_count: self.thoughts.len() as u64,
                agent_count: self.agent_registry.agents.len(),
                created_at,
                updated_at: now,
            },
        );
        save_mentisdb_registry(&metadata.chain_dir, &registry)
    }
}

impl Drop for MentisDb {
    fn drop(&mut self) {
        let _ = self.maybe_flush_agent_registry(true);
        let _ = self.maybe_flush_chain_registration(true);
    }
}

/// Stable filename derived from a chain key rather than mutable agent profile data.
///
/// # Example
///
/// ```
/// use mentisdb::chain_filename;
///
/// let a = chain_filename("agent1", "Researcher", Some("rust"), Some("careful"));
/// let b = chain_filename("agent1", "Different", Some("go"), Some("direct"));
/// let c = chain_filename("agent2", "Researcher", Some("rust"), Some("careful"));
///
/// assert_eq!(a, b);
/// assert_ne!(a, c);
/// ```
pub fn chain_filename(
    chain_key: &str,
    _agent_name: &str,
    _expertise: Option<&str>,
    _personality: Option<&str>,
) -> String {
    chain_storage_filename(chain_key, StorageAdapterKind::default())
}

/// Stable filename derived from a chain key and storage adapter kind.
///
/// # Example
///
/// ```
/// use mentisdb::{chain_storage_filename, StorageAdapterKind};
///
/// let jsonl = chain_storage_filename("agent1", StorageAdapterKind::Jsonl);
/// let binary = chain_storage_filename("agent1", StorageAdapterKind::Binary);
///
/// assert!(jsonl.ends_with(".jsonl"));
/// assert!(binary.ends_with(".tcbin"));
/// ```
pub fn chain_storage_filename(chain_key: &str, kind: StorageAdapterKind) -> String {
    let mut hasher = Sha256::new();
    hasher.update(chain_key.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let fingerprint = &digest[..16];

    let safe_key: String = chain_key
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();

    format!("{safe_key}-{fingerprint}.{}", kind.file_extension())
}

/// Recover the stable chain key portion from a MentisDb storage filename.
///
/// This reverses the filename convention used by [`chain_storage_filename`]
/// and returns the durable chain key prefix as stored in the filename. The
/// returned value matches the filename-safe key, so callers should treat it as
/// the persisted chain identifier rather than as an exact reconstruction of an
/// arbitrary original input string.
///
/// # Example
///
/// ```
/// use mentisdb::{chain_key_from_storage_filename, chain_storage_filename, StorageAdapterKind};
///
/// let filename = chain_storage_filename("borganism-brain", StorageAdapterKind::Jsonl);
/// let chain_key = chain_key_from_storage_filename(&filename).unwrap();
///
/// assert_eq!(chain_key, "borganism-brain");
/// ```
pub fn chain_key_from_storage_filename(filename: &str) -> Option<String> {
    let (stem, extension) = filename.rsplit_once('.')?;
    if extension != StorageAdapterKind::Jsonl.file_extension()
        && extension != StorageAdapterKind::Binary.file_extension()
    {
        return None;
    }

    let (chain_key, fingerprint) = stem.rsplit_once('-')?;
    if fingerprint.len() != 16
        || !fingerprint
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }

    Some(chain_key.to_string())
}

fn derive_persistence_metadata(storage: &dyn StorageAdapter) -> Option<ChainPersistenceMetadata> {
    let storage_path = storage.storage_path()?;
    let file_name = storage_path.file_name()?.to_str()?;
    let chain_key = chain_key_from_storage_filename(file_name)?;
    Some(ChainPersistenceMetadata {
        chain_key,
        chain_dir: storage_path.parent()?.to_path_buf(),
        storage_kind: storage.storage_kind(),
    })
}

fn mentisdb_registry_path(chain_dir: &Path) -> PathBuf {
    chain_dir.join(MENTISDB_REGISTRY_FILENAME)
}

fn legacy_thoughtchain_registry_path(chain_dir: &Path) -> PathBuf {
    chain_dir.join(LEGACY_THOUGHTCHAIN_REGISTRY_FILENAME)
}

fn resolve_registry_path(chain_dir: &Path) -> io::Result<PathBuf> {
    let mentisdb_path = mentisdb_registry_path(chain_dir);
    if mentisdb_path.exists() {
        return Ok(mentisdb_path);
    }

    let legacy_path = legacy_thoughtchain_registry_path(chain_dir);
    if legacy_path.exists() {
        fs::rename(&legacy_path, &mentisdb_path)?;
        return Ok(mentisdb_path);
    }

    Ok(mentisdb_path)
}

fn vector_search_error_to_io<E: fmt::Display>(error: VectorSearchError<E>) -> io::Error {
    match error {
        VectorSearchError::Io(error) => error,
        VectorSearchError::MissingPersistenceMetadata => io::Error::new(
            io::ErrorKind::Unsupported,
            "this MentisDb handle does not expose stable persistence metadata",
        ),
        other => io::Error::other(other.to_string()),
    }
}

fn chain_agent_registry_path(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
) -> PathBuf {
    let storage_file = chain_storage_filename(chain_key, storage_kind);
    let stem = storage_file
        .strip_suffix(&format!(".{}", storage_kind.file_extension()))
        .unwrap_or(&storage_file);
    chain_dir.join(format!("{stem}.agents.json"))
}

fn chain_vector_sidecar_path(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
    metadata: &crate::search::EmbeddingMetadata,
) -> PathBuf {
    let storage_file = chain_storage_filename(chain_key, storage_kind);
    let stem = storage_file
        .strip_suffix(&format!(".{}", storage_kind.file_extension()))
        .unwrap_or(&storage_file);
    let model = sanitize_sidecar_component(&metadata.model_id);
    let version = sanitize_sidecar_component(&metadata.embedding_version);
    chain_dir.join(format!(
        "{stem}.vectors.{model}.{version}.{}d.json",
        metadata.dimension
    ))
}

fn chain_vector_sidecar_config_path(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
) -> PathBuf {
    let storage_file = chain_storage_filename(chain_key, storage_kind);
    let stem = storage_file
        .strip_suffix(&format!(".{}", storage_kind.file_extension()))
        .unwrap_or(&storage_file);
    chain_dir.join(format!("{stem}.vectors.managed.json"))
}

fn sanitize_sidecar_component(value: &str) -> String {
    let safe: String = value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' => character.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    let trimmed = safe.trim_matches('-');
    if trimmed.is_empty() {
        "default".to_string()
    } else {
        trimmed.to_string()
    }
}

fn normalize_managed_vector_sidecar_configs(
    providers: Vec<ManagedVectorSidecarConfig>,
) -> Vec<ManagedVectorSidecarConfig> {
    let mut normalized = BTreeMap::new();
    for provider in providers {
        normalized.insert(provider.provider_kind, provider.enabled);
    }
    if normalized.is_empty() {
        normalized.insert(ManagedVectorProviderKind::LocalTextV1, true);
    }
    normalized
        .into_iter()
        .map(|(provider_kind, enabled)| ManagedVectorSidecarConfig {
            provider_kind,
            enabled,
        })
        .collect()
}

fn load_managed_vector_sidecar_config(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
) -> io::Result<ManagedVectorSidecarConfigFile> {
    let path = chain_vector_sidecar_config_path(chain_dir, chain_key, storage_kind);
    if !path.exists() {
        return Ok(ManagedVectorSidecarConfigFile::default());
    }
    let file = File::open(path)?;
    let mut config: ManagedVectorSidecarConfigFile = serde_json::from_reader(BufReader::new(file))
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize managed vector sidecar config: {error}"),
            )
        })?;
    if config.version != MANAGED_VECTOR_SIDECAR_CONFIG_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Unsupported managed vector sidecar config version {}",
                config.version
            ),
        ));
    }
    config.providers = normalize_managed_vector_sidecar_configs(config.providers);
    Ok(config)
}

fn save_managed_vector_sidecar_config(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
    config: &ManagedVectorSidecarConfigFile,
) -> io::Result<()> {
    let path = chain_vector_sidecar_config_path(chain_dir, chain_key, storage_kind);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer(writer, config).map_err(|error| {
        io::Error::other(format!(
            "Failed to serialize managed vector sidecar config: {error}"
        ))
    })
}

fn load_agent_registry(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
) -> io::Result<AgentRegistry> {
    let path = chain_agent_registry_path(chain_dir, chain_key, storage_kind);
    if !path.exists() {
        return Ok(AgentRegistry::default());
    }

    let file = fs::File::open(path)?;
    serde_json::from_reader(file).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to deserialize agent registry: {error}"),
        )
    })
}

fn save_agent_registry(
    chain_dir: &Path,
    chain_key: &str,
    storage_kind: StorageAdapterKind,
    registry: &AgentRegistry,
) -> io::Result<()> {
    let path = chain_agent_registry_path(chain_dir, chain_key, storage_kind);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer(writer, registry)
        .map_err(|error| io::Error::other(format!("Failed to serialize agent registry: {error}")))
}

fn load_mentisdb_registry(chain_dir: &Path) -> io::Result<MentisDbRegistry> {
    let path = resolve_registry_path(chain_dir)?;
    if !path.exists() {
        return Ok(MentisDbRegistry {
            version: MENTISDB_CURRENT_VERSION,
            chains: BTreeMap::new(),
        });
    }

    let file = fs::File::open(path)?;
    let mut registry: MentisDbRegistry = serde_json::from_reader(file).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to deserialize MentisDB registry: {error}"),
        )
    })?;
    // Always normalise the in-memory version to the current schema version so
    // callers that compare against `MENTISDB_CURRENT_VERSION` see a consistent
    // value regardless of when the file was last written.
    registry.version = MENTISDB_CURRENT_VERSION;
    Ok(registry)
}

fn save_mentisdb_registry(chain_dir: &Path, registry: &MentisDbRegistry) -> io::Result<()> {
    let path = mentisdb_registry_path(chain_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(path)?;
    serde_json::to_writer_pretty(file, registry).map_err(|error| {
        io::Error::other(format!("Failed to serialize MentisDB registry: {error}"))
    })?;

    let legacy_path = legacy_thoughtchain_registry_path(chain_dir);
    if legacy_path.exists() {
        fs::remove_file(legacy_path)?;
    }

    Ok(())
}

fn resolve_storage_kind_for_chain(
    chain_dir: &Path,
    chain_key: &str,
    default_kind: StorageAdapterKind,
) -> io::Result<StorageAdapterKind> {
    let registry = load_mentisdb_registry(chain_dir)?;
    if let Some(entry) = registry.chains.get(chain_key) {
        return Ok(entry.storage_adapter);
    }

    let jsonl_exists = chain_dir
        .join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl))
        .exists();
    let binary_exists = chain_dir
        .join(chain_storage_filename(
            chain_key,
            StorageAdapterKind::Binary,
        ))
        .exists();

    match (jsonl_exists, binary_exists) {
        (true, false) => Ok(StorageAdapterKind::Jsonl),
        (false, true) => Ok(StorageAdapterKind::Binary),
        (false, false) => Ok(default_kind),
        (true, true) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Conflicting storage files exist for chain '{chain_key}' without a registry entry"
            ),
        )),
    }
}

/// Load the registry of all known thought chains for a storage directory.
pub fn load_registered_chains<P: AsRef<Path>>(chain_dir: P) -> io::Result<MentisDbRegistry> {
    load_mentisdb_registry(chain_dir.as_ref())
}

/// Remove a chain from the registry and delete its storage file from disk.
///
/// If the chain is not registered this is a no-op. The in-memory cache held by
/// a running daemon must be purged separately by the caller.
pub fn deregister_chain<P: AsRef<Path>>(chain_dir: P, chain_key: &str) -> io::Result<()> {
    let chain_dir = chain_dir.as_ref();
    let mut registry = load_mentisdb_registry(chain_dir)?;
    if let Some(entry) = registry.chains.remove(chain_key) {
        save_mentisdb_registry(chain_dir, &registry)?;
        let storage_path = PathBuf::from(&entry.storage_location);
        if storage_path.exists() {
            fs::remove_file(&storage_path)?;
        }
        let agent_registry_path =
            chain_agent_registry_path(chain_dir, chain_key, entry.storage_adapter);
        if agent_registry_path.exists() {
            fs::remove_file(agent_registry_path)?;
        }
        let vector_config_path =
            chain_vector_sidecar_config_path(chain_dir, chain_key, entry.storage_adapter);
        if vector_config_path.exists() {
            fs::remove_file(vector_config_path)?;
        }
        let storage_file = chain_storage_filename(chain_key, entry.storage_adapter);
        let stem = storage_file
            .strip_suffix(&format!(".{}", entry.storage_adapter.file_extension()))
            .unwrap_or(&storage_file);
        let vector_prefix = format!("{stem}.vectors.");
        for entry in fs::read_dir(chain_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with(&vector_prefix) && file_name.ends_with(".json") {
                let path = entry.path();
                if path.is_file() {
                    fs::remove_file(path)?;
                }
            }
        }
    }
    Ok(())
}

/// Refresh stale `thought_count` and `agent_count` values in the on-disk registry
/// by opening each registered chain and reading live counts.
///
/// Call this once at daemon startup to repair any counts that went stale between
/// runs (e.g. from older versions, hard crashes, or chains appended outside the
/// running daemon). Only writes the registry file when at least one entry changes.
pub fn refresh_registered_chain_counts<P: AsRef<Path>>(chain_dir: P) -> io::Result<()> {
    let chain_dir = chain_dir.as_ref();
    let mut registry = load_mentisdb_registry(chain_dir)?;
    if registry.chains.is_empty() {
        return Ok(());
    }
    let now = Utc::now();
    let mut changed = false;
    for entry in registry.chains.values_mut() {
        let storage = entry
            .storage_adapter
            .for_chain_key(chain_dir, &entry.chain_key);
        if let Ok(chain) = MentisDb::open_with_storage(storage) {
            let live_thoughts = chain.thoughts().len() as u64;
            let live_agents = chain.agent_registry().agents.len();
            if entry.thought_count != live_thoughts || entry.agent_count != live_agents {
                entry.thought_count = live_thoughts;
                entry.agent_count = live_agents;
                entry.updated_at = now;
                changed = true;
            }
        }
    }
    if changed {
        save_mentisdb_registry(chain_dir, &registry)?;
    }
    Ok(())
}

/// Migrate all legacy v0 chain files in a storage directory to the current format.
pub fn migrate_registered_chains<P, F>(
    chain_dir: P,
    progress: F,
) -> io::Result<Vec<MentisDbMigrationReport>>
where
    P: AsRef<Path>,
    F: FnMut(MentisDbMigrationEvent),
{
    migrate_registered_chains_with_adapter(chain_dir, StorageAdapterKind::default(), progress)
}

/// Migrate all legacy v0 chain files in a storage directory to the current format
/// and target storage adapter.
pub fn migrate_registered_chains_with_adapter<P, F>(
    chain_dir: P,
    target_storage_adapter: StorageAdapterKind,
    mut progress: F,
) -> io::Result<Vec<MentisDbMigrationReport>>
where
    P: AsRef<Path>,
    F: FnMut(MentisDbMigrationEvent),
{
    let chain_dir = chain_dir.as_ref();
    fs::create_dir_all(chain_dir)?;
    let mut registry = load_mentisdb_registry(chain_dir)?;
    let mut discovered = discover_chain_files(chain_dir)?;
    discovered.sort_by(|left, right| left.chain_key.cmp(&right.chain_key));
    let pending: Vec<DiscoveredChainFile> = discovered
        .into_iter()
        .filter(|candidate| {
            registry
                .chains
                .get(&candidate.chain_key)
                .map(|entry| entry.version < MENTISDB_CURRENT_VERSION)
                .unwrap_or(true)
        })
        .collect();

    let total = pending.len();
    let mut reports = Vec::new();

    for (position, candidate) in pending.into_iter().enumerate() {
        let current = position + 1;
        progress(MentisDbMigrationEvent::Started {
            chain_key: candidate.chain_key.clone(),
            from_version: MENTISDB_SCHEMA_V0,
            to_version: MENTISDB_CURRENT_VERSION,
            current,
            total,
        });

        let report = migrate_legacy_chain_v0(chain_dir, &candidate, target_storage_adapter)?;
        upsert_chain_registration_from_report(chain_dir, &mut registry, &report)?;
        save_mentisdb_registry(chain_dir, &registry)?;
        progress(MentisDbMigrationEvent::Completed {
            chain_key: report.chain_key.clone(),
            from_version: report.from_version,
            to_version: report.to_version,
            current,
            total,
        });
        reports.push(report);
    }

    let discovered = discover_chain_files(chain_dir)?;
    let mut discovered_by_key: BTreeMap<String, Vec<DiscoveredChainFile>> = BTreeMap::new();
    for candidate in discovered {
        discovered_by_key
            .entry(candidate.chain_key.clone())
            .or_default()
            .push(candidate);
    }

    let reconciliation_candidates: Vec<String> = registry
        .chains
        .keys()
        .filter(|chain_key| {
            chain_needs_reconciliation(
                chain_dir,
                chain_key,
                registry.chains.get(*chain_key),
                discovered_by_key
                    .get(*chain_key)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                target_storage_adapter,
            )
        })
        .cloned()
        .collect();

    let reconciliation_total = reconciliation_candidates.len();
    for (position, chain_key) in reconciliation_candidates.into_iter().enumerate() {
        let current = position + 1;
        let discovered = discovered_by_key
            .get(&chain_key)
            .cloned()
            .unwrap_or_default();
        let source_storage_adapter = select_reconciliation_source(
            chain_dir,
            &chain_key,
            registry.chains.get(&chain_key),
            &discovered,
            target_storage_adapter,
        )?
        .map(|(candidate, _)| candidate.storage_kind)
        .unwrap_or(target_storage_adapter);

        progress(MentisDbMigrationEvent::StartedReconciliation {
            chain_key: chain_key.clone(),
            from_storage_adapter: source_storage_adapter,
            to_storage_adapter: target_storage_adapter,
            current,
            total: reconciliation_total,
        });

        if let Some(report) = reconcile_current_chain(
            chain_dir,
            &chain_key,
            registry.chains.get(&chain_key),
            &discovered,
            target_storage_adapter,
        )? {
            upsert_chain_registration_from_report(chain_dir, &mut registry, &report)?;
            save_mentisdb_registry(chain_dir, &registry)?;
            progress(MentisDbMigrationEvent::CompletedReconciliation {
                chain_key: report.chain_key.clone(),
                from_storage_adapter: report.source_storage_adapter,
                to_storage_adapter: report.storage_adapter,
                current,
                total: reconciliation_total,
            });
            reports.push(report);
        }
    }

    Ok(reports)
}

#[derive(Debug, Clone)]
struct DiscoveredChainFile {
    chain_key: String,
    storage_kind: StorageAdapterKind,
    path: PathBuf,
}

fn discover_chain_files(chain_dir: &Path) -> io::Result<Vec<DiscoveredChainFile>> {
    let mut discovered = Vec::new();
    for entry in fs::read_dir(chain_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(chain_key) = chain_key_from_storage_filename(file_name) else {
            continue;
        };
        let storage_kind = if file_name.ends_with(StorageAdapterKind::Jsonl.file_extension()) {
            StorageAdapterKind::Jsonl
        } else if file_name.ends_with(StorageAdapterKind::Binary.file_extension()) {
            StorageAdapterKind::Binary
        } else {
            continue;
        };
        discovered.push(DiscoveredChainFile {
            chain_key,
            storage_kind,
            path: entry.path(),
        });
    }
    Ok(discovered)
}

fn storage_adapter_for_path(
    storage_kind: StorageAdapterKind,
    path: &Path,
) -> Box<dyn StorageAdapter> {
    match storage_kind {
        StorageAdapterKind::Jsonl => Box::new(JsonlStorageAdapter::new(path.to_path_buf())),
        StorageAdapterKind::Binary => Box::new(BinaryStorageAdapter::new(path.to_path_buf())),
    }
}

fn open_current_chain_at(path: &Path, storage_kind: StorageAdapterKind) -> io::Result<MentisDb> {
    MentisDb::open_with_storage(storage_adapter_for_path(storage_kind, path))
}

fn persist_thoughts_to_path(
    path: &Path,
    storage_kind: StorageAdapterKind,
    thoughts: &[Thought],
) -> io::Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }

    for thought in thoughts {
        match storage_kind {
            StorageAdapterKind::Jsonl => persist_jsonl_thought(path, thought)?,
            StorageAdapterKind::Binary => persist_binary_thought(path, thought)?,
        }
    }

    Ok(())
}

fn archive_chain_file(
    chain_dir: &Path,
    source_path: &Path,
    from_version: u32,
    to_version: u32,
) -> io::Result<PathBuf> {
    let archive_dir = chain_dir
        .join("migrations")
        .join(format!("v{}_to_v{}", from_version, to_version));
    fs::create_dir_all(&archive_dir)?;
    let archived_path = archive_dir.join(
        source_path
            .file_name()
            .map(|value| value.to_owned())
            .unwrap_or_default(),
    );
    if archived_path.exists() {
        fs::remove_file(&archived_path)?;
    }
    fs::rename(source_path, &archived_path)?;
    Ok(archived_path)
}

fn upsert_chain_registration_from_report(
    chain_dir: &Path,
    registry: &mut MentisDbRegistry,
    report: &MentisDbMigrationReport,
) -> io::Result<()> {
    let now = Utc::now();
    let created_at = registry
        .chains
        .get(&report.chain_key)
        .map(|entry| entry.created_at)
        .unwrap_or(now);
    registry.chains.insert(
        report.chain_key.clone(),
        MentisDbRegistration {
            chain_key: report.chain_key.clone(),
            version: report.to_version,
            storage_adapter: report.storage_adapter,
            storage_location: chain_dir
                .join(chain_storage_filename(
                    &report.chain_key,
                    report.storage_adapter,
                ))
                .display()
                .to_string(),
            thought_count: report.thought_count,
            agent_count: load_agent_registry(chain_dir, &report.chain_key, report.storage_adapter)?
                .agents
                .len(),
            created_at,
            updated_at: now,
        },
    );
    Ok(())
}

fn chain_needs_reconciliation(
    chain_dir: &Path,
    chain_key: &str,
    registration: Option<&MentisDbRegistration>,
    discovered: &[DiscoveredChainFile],
    target_storage_adapter: StorageAdapterKind,
) -> bool {
    let Some(registration) = registration else {
        return false;
    };

    if registration.version < MENTISDB_CURRENT_VERSION {
        return false;
    }

    let expected_path = chain_dir.join(chain_storage_filename(chain_key, target_storage_adapter));
    if registration.storage_adapter != target_storage_adapter {
        return true;
    }
    if !expected_path.exists() {
        return true;
    }
    if open_current_chain_at(&expected_path, target_storage_adapter).is_err() {
        return true;
    }

    discovered
        .iter()
        .any(|candidate| candidate.path != expected_path)
}

fn select_reconciliation_source(
    chain_dir: &Path,
    chain_key: &str,
    registration: Option<&MentisDbRegistration>,
    discovered: &[DiscoveredChainFile],
    target_storage_adapter: StorageAdapterKind,
) -> io::Result<Option<(DiscoveredChainFile, MentisDb)>> {
    let mut candidates = discovered.to_vec();
    candidates.sort_by_key(|candidate| {
        if registration
            .map(|entry| entry.storage_adapter == candidate.storage_kind)
            .unwrap_or(false)
        {
            0
        } else if candidate.storage_kind == target_storage_adapter {
            1
        } else {
            2
        }
    });

    for candidate in candidates {
        if let Ok(chain) = open_current_chain_at(&candidate.path, candidate.storage_kind) {
            return Ok(Some((candidate, chain)));
        }
    }

    let expected_path = chain_dir.join(chain_storage_filename(chain_key, target_storage_adapter));
    if expected_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "No valid MentisDb source found to repair chain '{chain_key}' at {}",
                expected_path.display()
            ),
        ));
    }

    Ok(None)
}

fn reconcile_current_chain(
    chain_dir: &Path,
    chain_key: &str,
    registration: Option<&MentisDbRegistration>,
    discovered: &[DiscoveredChainFile],
    target_storage_adapter: StorageAdapterKind,
) -> io::Result<Option<MentisDbMigrationReport>> {
    let expected_path = chain_dir.join(chain_storage_filename(chain_key, target_storage_adapter));
    let Some((source, chain)) = select_reconciliation_source(
        chain_dir,
        chain_key,
        registration,
        discovered,
        target_storage_adapter,
    )?
    else {
        return Ok(None);
    };

    let source_is_target =
        source.storage_kind == target_storage_adapter && source.path == expected_path;
    let target_missing = !expected_path.exists();
    let target_invalid = expected_path.exists()
        && open_current_chain_at(&expected_path, target_storage_adapter).is_err();
    let has_extra_files = discovered
        .iter()
        .any(|candidate| candidate.path != expected_path);
    if source_is_target && !target_missing && !target_invalid && !has_extra_files {
        return Ok(None);
    }

    let temp_path =
        expected_path.with_extension(format!("{}.tmp", target_storage_adapter.file_extension()));
    if temp_path.exists() {
        fs::remove_file(&temp_path)?;
    }
    persist_thoughts_to_path(&temp_path, target_storage_adapter, chain.thoughts())?;
    save_agent_registry(
        chain_dir,
        chain_key,
        target_storage_adapter,
        chain.agent_registry(),
    )?;

    if expected_path.exists() {
        fs::remove_file(&expected_path)?;
    }
    fs::rename(&temp_path, &expected_path)?;

    let mut archived_path = None;
    for candidate in discovered {
        if candidate.path == expected_path {
            continue;
        }
        if candidate.path.exists() {
            let archived = archive_chain_file(
                chain_dir,
                &candidate.path,
                MENTISDB_CURRENT_VERSION,
                MENTISDB_CURRENT_VERSION,
            )?;
            if archived_path.is_none() {
                archived_path = Some(archived);
            }
        }
    }

    Ok(Some(MentisDbMigrationReport {
        chain_key: chain_key.to_string(),
        from_version: MENTISDB_CURRENT_VERSION,
        to_version: MENTISDB_CURRENT_VERSION,
        source_storage_adapter: source.storage_kind,
        storage_adapter: target_storage_adapter,
        thought_count: chain.thoughts().len() as u64,
        archived_legacy_path: archived_path,
    }))
}

fn migrate_legacy_chain_v0(
    chain_dir: &Path,
    discovered: &DiscoveredChainFile,
    target_storage_adapter: StorageAdapterKind,
) -> io::Result<MentisDbMigrationReport> {
    let legacy_thoughts = load_legacy_v0_thoughts(&discovered.path, discovered.storage_kind)?;
    let (thoughts, agent_registry) = migrate_legacy_thoughts(legacy_thoughts);
    let active_path = chain_dir.join(chain_storage_filename(
        &discovered.chain_key,
        target_storage_adapter,
    ));
    let temp_path =
        active_path.with_extension(format!("{}.tmp", target_storage_adapter.file_extension()));
    if temp_path.exists() {
        fs::remove_file(&temp_path)?;
    }

    for thought in &thoughts {
        match target_storage_adapter {
            StorageAdapterKind::Jsonl => persist_jsonl_thought(&temp_path, thought)?,
            StorageAdapterKind::Binary => persist_binary_thought(&temp_path, thought)?,
        }
    }

    save_agent_registry(
        chain_dir,
        &discovered.chain_key,
        target_storage_adapter,
        &agent_registry,
    )?;

    let archived_legacy_path = archive_chain_file(
        chain_dir,
        &discovered.path,
        MENTISDB_SCHEMA_V0,
        MENTISDB_CURRENT_VERSION,
    )?;
    fs::rename(&temp_path, &active_path)?;

    Ok(MentisDbMigrationReport {
        chain_key: discovered.chain_key.clone(),
        from_version: MENTISDB_SCHEMA_V0,
        to_version: MENTISDB_CURRENT_VERSION,
        source_storage_adapter: discovered.storage_kind,
        storage_adapter: target_storage_adapter,
        thought_count: thoughts.len() as u64,
        archived_legacy_path: Some(archived_legacy_path),
    })
}

fn load_legacy_v0_thoughts(
    file_path: &Path,
    storage_kind: StorageAdapterKind,
) -> io::Result<Vec<LegacyThoughtV0>> {
    match storage_kind {
        StorageAdapterKind::Jsonl => load_legacy_v0_jsonl_thoughts(file_path),
        StorageAdapterKind::Binary => load_legacy_v0_binary_thoughts(file_path),
    }
}

fn load_legacy_v0_jsonl_thoughts(file_path: &Path) -> io::Result<Vec<LegacyThoughtV0>> {
    if !file_path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let thought: LegacyThoughtV0 = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse legacy v0 thought: {error}"),
            )
        })?;
        entries.push(thought);
    }
    Ok(entries)
}

fn load_legacy_v0_binary_thoughts(file_path: &Path) -> io::Result<Vec<LegacyThoughtV0>> {
    if !file_path.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(file_path)?;
    let mut thoughts = Vec::new();

    loop {
        let mut length_bytes = [0_u8; 8];
        match file.read_exact(&mut length_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        }

        const MAX_THOUGHT_PAYLOAD_BYTES: u64 = 10 * 1024 * 1024; // DoS protection: 10 MB limit
        let length_u64 = u64::from_le_bytes(length_bytes);
        if length_u64 > MAX_THOUGHT_PAYLOAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("binary thought payload length {length_u64} exceeds maximum {MAX_THOUGHT_PAYLOAD_BYTES}"),
            ));
        }
        let length = length_u64 as usize;
        let mut payload = vec![0_u8; length];
        file.read_exact(&mut payload)?;
        let (thought, _): (LegacyThoughtV0, usize) =
            bincode::serde::decode_from_slice(&payload, bincode::config::standard()).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to deserialize legacy v0 binary thought: {error}"),
                    )
                },
            )?;
        thoughts.push(thought);
    }

    Ok(thoughts)
}

fn migrate_legacy_thoughts(legacy_thoughts: Vec<LegacyThoughtV0>) -> (Vec<Thought>, AgentRegistry) {
    let mut migrated = Vec::with_capacity(legacy_thoughts.len());
    let mut agent_registry = AgentRegistry::default();
    let mut prev_hash = String::new();

    for legacy in legacy_thoughts {
        let thought = Thought {
            schema_version: MENTISDB_CURRENT_VERSION,
            id: legacy.id,
            index: legacy.index,
            timestamp: legacy.timestamp,
            session_id: legacy.session_id,
            agent_id: legacy.agent_id.clone(),
            signing_key_id: legacy.signing_key_id,
            thought_signature: legacy.thought_signature,
            thought_type: legacy.thought_type,
            role: legacy.role,
            content: legacy.content,
            confidence: legacy.confidence,
            importance: legacy.importance,
            tags: legacy.tags,
            concepts: legacy.concepts,
            refs: legacy.refs,
            relations: legacy
                .relations
                .into_iter()
                .map(ThoughtRelation::from)
                .collect(),
            prev_hash: prev_hash.clone(),
            hash: String::new(),
        };
        let hash = compute_thought_hash(&thought);
        let thought = Thought { hash, ..thought };
        prev_hash = thought.hash.clone();
        agent_registry.observe(&legacy.agent_id, None, None, legacy.index, legacy.timestamp);
        migrated.push(thought);
    }

    (migrated, agent_registry)
}

// ---------------------------------------------------------------------------
// MEMORY.md import helpers
// ---------------------------------------------------------------------------

/// Parsed representation of a single MEMORY.md thought line.
struct MarkdownThoughtLine {
    thought_type: ThoughtType,
    content: String,
    agent_id: Option<String>,
    role: ThoughtRole,
    importance: f32,
    confidence: Option<f32>,
    tags: Vec<String>,
}

/// Parse a [`ThoughtType`] from its `{:?}` (Debug) PascalCase representation,
/// accepting any mix of case and ignoring non-alphanumeric characters.
fn parse_thought_type_from_debug(input: &str) -> Option<ThoughtType> {
    let normalized: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    match normalized.as_str() {
        "preferenceupdate" => Some(ThoughtType::PreferenceUpdate),
        "usertrait" => Some(ThoughtType::UserTrait),
        "relationshipupdate" => Some(ThoughtType::RelationshipUpdate),
        "finding" => Some(ThoughtType::Finding),
        "insight" => Some(ThoughtType::Insight),
        "factlearned" => Some(ThoughtType::FactLearned),
        "patterndetected" => Some(ThoughtType::PatternDetected),
        "hypothesis" => Some(ThoughtType::Hypothesis),
        "mistake" => Some(ThoughtType::Mistake),
        "correction" => Some(ThoughtType::Correction),
        "lessonlearned" => Some(ThoughtType::LessonLearned),
        "assumptioninvalidated" => Some(ThoughtType::AssumptionInvalidated),
        "constraint" => Some(ThoughtType::Constraint),
        "plan" => Some(ThoughtType::Plan),
        "subgoal" => Some(ThoughtType::Subgoal),
        "decision" => Some(ThoughtType::Decision),
        "strategyshift" => Some(ThoughtType::StrategyShift),
        "wonder" => Some(ThoughtType::Wonder),
        "question" => Some(ThoughtType::Question),
        "idea" => Some(ThoughtType::Idea),
        "experiment" => Some(ThoughtType::Experiment),
        "actiontaken" => Some(ThoughtType::ActionTaken),
        "taskcomplete" => Some(ThoughtType::TaskComplete),
        "checkpoint" => Some(ThoughtType::Checkpoint),
        "statesnapshot" => Some(ThoughtType::StateSnapshot),
        "handoff" => Some(ThoughtType::Handoff),
        "summary" => Some(ThoughtType::Summary),
        "reframe" => Some(ThoughtType::Reframe),
        "surprise" => Some(ThoughtType::Surprise),
        _ => None,
    }
}

/// Parse a [`ThoughtRole`] from its `{:?}` (Debug) PascalCase representation.
fn parse_thought_role_from_debug(input: &str) -> Option<ThoughtRole> {
    let normalized: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    match normalized.as_str() {
        "memory" => Some(ThoughtRole::Memory),
        "workingmemory" => Some(ThoughtRole::WorkingMemory),
        "summary" => Some(ThoughtRole::Summary),
        "compression" => Some(ThoughtRole::Compression),
        "checkpoint" => Some(ThoughtRole::Checkpoint),
        "handoff" => Some(ThoughtRole::Handoff),
        "audit" => Some(ThoughtRole::Audit),
        "retrospective" => Some(ThoughtRole::Retrospective),
        _ => None,
    }
}

/// Find the byte position of the ` (` that begins the metadata block.
///
/// The metadata block always has `agent ` as its first token, so we search
/// for the last ` (agent ` occurrence (handling edge cases where the thought
/// content itself contains parentheses).
fn find_metadata_start(s: &str) -> Option<usize> {
    if !s.ends_with(')') {
        return None;
    }
    let mut search_from = 0;
    let mut last_found = None;
    while let Some(rel_pos) = s[search_from..].find(" (") {
        let abs_pos = search_from + rel_pos;
        if s[abs_pos + 2..].starts_with("agent ") {
            last_found = Some(abs_pos);
        }
        search_from = abs_pos + 1;
    }
    last_found
}

/// Parse a metadata block of the form:
/// `agent alice; role Retrospective; importance 0.75; confidence 0.90; tags tag1, tag2`
///
/// Returns `(agent_id, role, importance, confidence, tags)`.
fn parse_metadata_block(
    meta: &str,
) -> (Option<String>, ThoughtRole, f32, Option<f32>, Vec<String>) {
    let mut agent_id: Option<String> = None;
    let mut role = ThoughtRole::Memory;
    let mut importance = 0.5_f32;
    let mut confidence: Option<f32> = None;
    let mut tags: Vec<String> = Vec::new();

    for token in meta.split("; ") {
        let token = token.trim();
        if let Some(val) = token.strip_prefix("agent ") {
            agent_id = Some(val.trim().to_string());
        } else if let Some(val) = token.strip_prefix("role ") {
            role = parse_thought_role_from_debug(val.trim()).unwrap_or(ThoughtRole::Memory);
        } else if let Some(val) = token.strip_prefix("importance ") {
            importance = val.trim().parse().unwrap_or(0.5);
        } else if let Some(val) = token.strip_prefix("confidence ") {
            confidence = val.trim().parse().ok();
        } else if let Some(val) = token.strip_prefix("tags ") {
            tags = val
                .split(", ")
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    (agent_id, role, importance, confidence, tags)
}

/// Parse a single MEMORY.md bullet line of the format:
///
/// ```text
/// - [#N] TypeName: content (agent agent_id; role RoleName; importance 0.85; ...)
/// ```
///
/// Returns `None` for any line that does not match the expected pattern.
fn parse_memory_markdown_line(line: &str) -> Option<MarkdownThoughtLine> {
    let line = line.trim();

    // Strip leading "- [" prefix.
    let rest = line.strip_prefix("- [")?;

    // Skip past the index digit(s) and the closing "]".
    let bracket_close = rest.find(']')?;
    let after_bracket = rest.get(bracket_close + 1..)?.trim_start();

    // Split on the first ": " to get "TypeName" and "content (meta...)".
    let colon_pos = after_bracket.find(": ")?;
    let type_name = &after_bracket[..colon_pos];
    let content_and_meta = &after_bracket[colon_pos + 2..];

    let thought_type = parse_thought_type_from_debug(type_name)?;

    // Split content from metadata block.
    let (content, agent_id, role, importance, confidence, tags) =
        if let Some(meta_start) = find_metadata_start(content_and_meta) {
            let content = content_and_meta[..meta_start].trim();
            // Strip the leading " (" and trailing ")".
            let meta_inner = &content_and_meta[meta_start + 2..content_and_meta.len() - 1];
            let (agent_id, role, importance, confidence, tags) = parse_metadata_block(meta_inner);
            (content, agent_id, role, importance, confidence, tags)
        } else {
            (
                content_and_meta.trim(),
                None,
                ThoughtRole::Memory,
                0.5_f32,
                None,
                Vec::new(),
            )
        };

    if content.is_empty() {
        return None;
    }

    Some(MarkdownThoughtLine {
        thought_type,
        content: content.to_string(),
        agent_id,
        role,
        importance,
        confidence,
        tags,
    })
}

fn append_memory_section(
    markdown: &mut String,
    chain: &MentisDb,
    title: &str,
    thoughts: &[&Thought],
    types: &[ThoughtType],
) {
    let items: Vec<&Thought> = thoughts
        .iter()
        .copied()
        .filter(|thought| types.contains(&thought.thought_type))
        .collect();
    if items.is_empty() {
        return;
    }

    markdown.push_str(&format!("## {title}\n\n"));
    for thought in items {
        markdown.push_str(&format!(
            "- [#{}] {:?}: {}",
            thought.index, thought.thought_type, thought.content
        ));
        let mut metadata = Vec::new();
        metadata.push(format!("agent {}", chain.agent_label_for(thought)));
        if thought.role != ThoughtRole::Memory {
            metadata.push(format!("role {:?}", thought.role));
        }
        metadata.push(format!("importance {:.2}", thought.importance));
        if let Some(confidence) = thought.confidence {
            metadata.push(format!("confidence {:.2}", confidence));
        }
        if !thought.tags.is_empty() {
            metadata.push(format!("tags {}", thought.tags.join(", ")));
        }
        if !metadata.is_empty() {
            markdown.push_str(&format!(" ({})", metadata.join("; ")));
        }
        markdown.push('\n');
    }
    markdown.push('\n');
}

fn contains_case_insensitive(values: &[String], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    values
        .iter()
        .any(|value| value.to_lowercase() == needle || value.to_lowercase().contains(&needle))
}

fn equals_case_insensitive(value: &str, needle: &str) -> bool {
    value.eq_ignore_ascii_case(needle)
}

fn normalize_non_empty_label(value: &str, field_name: &str) -> io::Result<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{field_name} must not be empty"),
        ));
    }
    Ok(normalized.to_string())
}

fn normalize_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_lowercase();
        if seen.insert(key) {
            normalized.push(trimmed.to_string());
        }
    }
    normalized
}

fn union_position_lists<'a, I>(lists: I) -> Vec<usize>
where
    I: IntoIterator<Item = &'a Vec<usize>>,
{
    let mut positions: Vec<usize> = lists
        .into_iter()
        .flat_map(|entries| entries.iter().copied())
        .collect();
    positions.sort_unstable();
    positions.dedup();
    positions
}

fn matching_index_positions(index: &HashMap<String, Vec<usize>>, needles: &[String]) -> Vec<usize> {
    let normalized_needles: Vec<String> = needles
        .iter()
        .map(|needle| needle.trim().to_lowercase())
        .filter(|needle| !needle.is_empty())
        .collect();
    if normalized_needles.is_empty() {
        return Vec::new();
    }

    union_position_lists(index.iter().filter_map(|(value, positions)| {
        normalized_needles
            .iter()
            .any(|needle| value.contains(needle))
            .then_some(positions)
    }))
}

fn intersect_sorted_positions(left: &[usize], right: &[usize]) -> Vec<usize> {
    let mut result = Vec::with_capacity(left.len().min(right.len()));
    let mut left_index = 0;
    let mut right_index = 0;

    while left_index < left.len() && right_index < right.len() {
        match left[left_index].cmp(&right[right_index]) {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => {
                result.push(left[left_index]);
                left_index += 1;
                right_index += 1;
            }
        }
    }

    result
}

fn validate_refs(thoughts: &[Thought], refs: &[u64]) -> io::Result<()> {
    for &reference in refs {
        if reference as usize >= thoughts.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Thought reference {reference} does not exist"),
            ));
        }
    }
    Ok(())
}

fn dedupe_relations(relations: &mut Vec<ThoughtRelation>) {
    let mut seen = HashSet::new();
    relations.retain(|relation| seen.insert((relation.kind, relation.target_id)));
}

fn persist_jsonl_thought(file_path: &Path, thought: &Thought) -> io::Result<()> {
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    let json = serde_json::to_string(thought)
        .map_err(|error| io::Error::other(format!("Failed to serialize thought: {error}")))?;
    writeln!(file, "{json}")?;
    Ok(())
}

fn load_binary_thoughts(file_path: &Path) -> io::Result<Vec<Thought>> {
    if !file_path.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(file_path)?;
    let mut thoughts = Vec::new();

    loop {
        let mut length_bytes = [0_u8; 8];
        match file.read_exact(&mut length_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        }

        const MAX_THOUGHT_PAYLOAD_BYTES: u64 = 10 * 1024 * 1024; // DoS protection: 10 MB limit
        let length_u64 = u64::from_le_bytes(length_bytes);
        if length_u64 > MAX_THOUGHT_PAYLOAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("binary thought payload length {length_u64} exceeds maximum {MAX_THOUGHT_PAYLOAD_BYTES}"),
            ));
        }
        let length = length_u64 as usize;
        let mut payload = vec![0_u8; length];
        file.read_exact(&mut payload)?;
        let (thought, _bytes_read): (Thought, usize) =
            bincode::serde::decode_from_slice(&payload, bincode::config::standard()).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to deserialize binary thought: {error}"),
                    )
                },
            )?;
        thoughts.push(thought);
    }

    Ok(thoughts)
}

fn persist_binary_thought(file_path: &Path, thought: &Thought) -> io::Result<()> {
    let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard())
        .map_err(|error| io::Error::other(format!("Failed to serialize thought: {error}")))?;
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)?;
    file.write_all(&(payload.len() as u64).to_le_bytes())?;
    file.write_all(&payload)?;
    Ok(())
}

fn compute_thought_hash(thought: &Thought) -> String {
    #[derive(Serialize)]
    struct CanonicalThought<'a> {
        schema_version: u32,
        id: Uuid,
        index: u64,
        timestamp: &'a DateTime<Utc>,
        session_id: Option<Uuid>,
        agent_id: &'a str,
        signing_key_id: Option<&'a str>,
        thought_signature: Option<&'a [u8]>,
        thought_type: ThoughtType,
        role: ThoughtRole,
        content: &'a str,
        confidence: Option<f32>,
        importance: f32,
        tags: &'a [String],
        concepts: &'a [String],
        refs: &'a [u64],
        relations: &'a [ThoughtRelation],
        prev_hash: &'a str,
    }

    let canonical = CanonicalThought {
        schema_version: thought.schema_version,
        id: thought.id,
        index: thought.index,
        timestamp: &thought.timestamp,
        session_id: thought.session_id,
        agent_id: &thought.agent_id,
        signing_key_id: thought.signing_key_id.as_deref(),
        thought_signature: thought.thought_signature.as_deref(),
        thought_type: thought.thought_type,
        role: thought.role,
        content: &thought.content,
        confidence: thought.confidence,
        importance: thought.importance,
        tags: &thought.tags,
        concepts: &thought.concepts,
        refs: &thought.refs,
        relations: &thought.relations,
        prev_hash: &thought.prev_hash,
    };

    let bytes =
        serde_json::to_vec(&canonical).expect("canonical thought serialization should not fail");
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
