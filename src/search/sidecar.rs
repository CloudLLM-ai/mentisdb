//! Rebuildable vector sidecar persistence for one durable chain.
//!
//! Vector sidecars are derived artifacts keyed by one chain and one embedding
//! space. They never replace the append-only chain itself, and they can always
//! be rebuilt from the canonical thought log.
//!
//! # Persistence
//!
//! The durable snapshot is still a JSON file (`VectorSidecar`). Incremental
//! appends write one integrity-chained record to a sibling WAL
//! (`<snapshot>.json.wal`, magic `MDBVWAL1`). [`VectorSidecar::load_from_path`]
//! verifies the snapshot digest, then replays the WAL. After
//! [`wal_compact_threshold`] records (at least
//! [`VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD`], growing with the corpus so the
//! amortized cost per append stays constant) the WAL is folded into the JSON
//! snapshot. Binaries that only understand the JSON snapshot see a stale
//! sidecar when a WAL is pending and rebuild from the chain.
//!
//! # Compaction never runs under a chain lock
//!
//! Folding the WAL into the snapshot serializes and hashes the whole corpus
//! (`O(entries x dimension)`: seconds for 100 000 vectors of 384 floats), so a
//! service must never do it while holding the chain's write lock. A managed
//! chain therefore only *requests* a compaction; [`spawn_sidecar_compaction`]
//! runs [`compact_sidecar_snapshot`] on its own thread, working from the files
//! on disk. It re-reads the snapshot and the WAL, writes a new snapshot beside
//! the old one, and takes the caller's I/O lock only for the two short steps
//! that touch shared files (reading the WAL, swapping files).
//!
//! Appends keep chaining WAL records from the incremental digest while the
//! compaction runs, so the live handle never has to adopt the full-corpus
//! digest. The compacted snapshot records where the WAL chain continues in
//! [`VectorSidecar::wal_base_digest_hex`]: the snapshot's own `integrity` digest
//! still covers the whole corpus (and is verified on load), while WAL records
//! appended after the compaction point chain from `wal_base_digest_hex`.
//!
//! The compaction swaps the snapshot first and the WAL second. A crash between
//! the two leaves a snapshot that already holds records the WAL still lists;
//! WAL replay skips such leading records (they must be covered by the snapshot
//! and chain to its `wal_base_digest_hex`), so every interleaving recovers the
//! same sidecar. Older binaries ignore `wal_base_digest_hex`, fail to replay a
//! WAL that continues from it, and rebuild the derived sidecar from the chain.

use crate::search::flat_store::{VectorRow, VectorStore};
use crate::search::EmbeddingMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use uuid::Uuid;

/// Current schema version for persisted vector sidecars.
pub const VECTOR_SIDECAR_SCHEMA_VERSION: u32 = 1;
/// Compact the JSON snapshot after this many incremental WAL records.
pub const VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD: usize = 32;
/// A compaction rewrites and rehashes the whole corpus, so for large sidecars
/// it is requested once the WAL holds this fraction (1/N) of the entries.
const WAL_COMPACT_CORPUS_DIVISOR: usize = 16;

/// Number of pending WAL records at which a sidecar holding `entry_count`
/// entries should be compacted.
///
/// A fixed threshold made every 32nd append re-serialize and re-hash the
/// entire corpus (about 200 ms at 10 000 thoughts of 384 dimensions), so the
/// compaction work grew linearly with the chain. Scaling the threshold with the
/// corpus keeps the *amortized* cost per append constant; the WAL stays small
/// (records are binary) and replays quickly. The cost of one compaction is
/// still `O(entries x dimension)`, which is why it runs on its own thread
/// ([`spawn_sidecar_compaction`]) and never under a chain lock.
///
/// # Examples
///
/// ```
/// use mentisdb::search::{wal_compact_threshold, VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD};
///
/// assert_eq!(wal_compact_threshold(0), VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD);
/// assert_eq!(wal_compact_threshold(500), VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD);
/// assert_eq!(wal_compact_threshold(16_000), 1_000);
/// ```
pub fn wal_compact_threshold(entry_count: usize) -> usize {
    (entry_count / WAL_COMPACT_CORPUS_DIVISOR).max(VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD)
}
const WAL_MAGIC: &[u8; 8] = b"MDBVWAL1";

/// One persisted vector row for a committed thought.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorSidecarEntry {
    /// Stable UUID of the source thought.
    pub thought_id: Uuid,
    /// Stable append-order index of the source thought.
    pub thought_index: u64,
    /// Stable hash of the source thought.
    pub thought_hash: String,
    /// Dense vector in the sidecar's embedding space.
    ///
    /// A handle into the sidecar's flat [`VectorStore`], not a private
    /// allocation: the exact index, the HNSW graph and the implicit-edge scanner
    /// hold handles to the same rows, so every vector exists once in memory, and
    /// cloning an entry or a whole sidecar copies pointers, not floats. It
    /// dereferences to `&[f32]` and serializes as a plain JSON array of floats,
    /// so the on-disk format and the integrity digest are unchanged.
    pub vector: VectorRow,
}

impl VectorSidecarEntry {
    /// Create one persisted vector row. `vector` may be a `Vec<f32>` or a
    /// slice (copied into a one-row store of its own) or a [`VectorRow`]
    /// (shared as is). A sidecar moves the vector into its own store when the
    /// entry is added to it.
    pub fn new(
        thought_id: Uuid,
        thought_index: u64,
        thought_hash: impl Into<String>,
        vector: impl Into<VectorRow>,
    ) -> Self {
        Self {
            thought_id,
            thought_index,
            thought_hash: thought_hash.into(),
            vector: vector.into(),
        }
    }
}

/// Integrity metadata for one persisted sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorSidecarIntegrity {
    /// Integrity algorithm identifier.
    pub algorithm: String,
    /// Number of embedded entries included in the digest.
    pub entry_count: usize,
    /// Hex-encoded digest over the canonical payload.
    pub digest_hex: String,
}

impl VectorSidecarIntegrity {
    fn sha256(entry_count: usize, digest_hex: String) -> Self {
        Self {
            algorithm: "sha256".to_string(),
            entry_count,
            digest_hex,
        }
    }
}

/// Freshness state for one loaded vector sidecar relative to the live chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VectorSidecarFreshness {
    /// Sidecar metadata matches the current chain and embedding space.
    Fresh,
    /// Sidecar was built for a different chain key.
    ChainKeyMismatch {
        /// Expected live chain key.
        expected: String,
        /// Persisted chain key inside the sidecar.
        actual: String,
    },
    /// Sidecar was built for a different model identifier.
    ModelMismatch {
        /// Expected model identifier.
        expected: String,
        /// Actual model identifier.
        actual: String,
    },
    /// Sidecar was built for a different embedding-version label.
    EmbeddingVersionMismatch {
        /// Expected embedding version.
        expected: String,
        /// Actual embedding version.
        actual: String,
    },
    /// Sidecar was built for a different embedding dimension.
    DimensionMismatch {
        /// Expected embedding dimension.
        expected: usize,
        /// Actual embedding dimension.
        actual: usize,
    },
    /// Sidecar was built against a different thought count.
    StaleThoughtCount {
        /// Current thought count in the chain.
        expected: usize,
        /// Persisted thought count inside the sidecar.
        actual: usize,
    },
    /// Sidecar was built against a different chain head hash.
    StaleHeadHash {
        /// Current head hash in the chain.
        expected: Option<String>,
        /// Persisted head hash inside the sidecar.
        actual: Option<String>,
    },
}

/// Persisted vector sidecar for one chain and one embedding space.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorSidecar {
    /// Sidecar schema version.
    pub schema_version: u32,
    /// Durable chain key this sidecar belongs to.
    pub chain_key: String,
    /// Embedding-space metadata for all vectors in this sidecar.
    pub metadata: EmbeddingMetadata,
    /// Number of committed thoughts present when the sidecar was built.
    pub thought_count: usize,
    /// Head hash of the chain when the sidecar was built.
    pub head_hash: Option<String>,
    /// Timestamp when the sidecar was generated.
    pub generated_at: DateTime<Utc>,
    /// Integrity metadata for corruption detection.
    pub integrity: VectorSidecarIntegrity,
    /// Embedded rows ordered by append position.
    pub entries: Vec<VectorSidecarEntry>,
    /// Digest the WAL chain continues from, when it differs from
    /// [`VectorSidecarIntegrity::digest_hex`].
    ///
    /// Present only in a snapshot written by [`compact_sidecar_snapshot`]: the
    /// snapshot's `integrity` is the full-corpus digest, while WAL records
    /// appended before and after the compaction chain from the incremental
    /// digest at the compaction point. Loading adopts it as the chain head and
    /// clears the field, so an in-memory sidecar never carries it. Absent (and
    /// not serialized) in every other snapshot, where the WAL chains from
    /// `integrity.digest_hex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wal_base_digest_hex: Option<String>,
    /// Where the rows of `entries` live (see [`VectorStore`]). Never persisted.
    #[serde(skip)]
    store: VectorStore,
}

impl VectorSidecar {
    /// Build a validated sidecar from derived vector entries.
    pub fn build(
        chain_key: impl Into<String>,
        metadata: EmbeddingMetadata,
        thought_count: usize,
        head_hash: Option<String>,
        generated_at: DateTime<Utc>,
        entries: Vec<VectorSidecarEntry>,
    ) -> io::Result<Self> {
        let mut sidecar = Self {
            schema_version: VECTOR_SIDECAR_SCHEMA_VERSION,
            chain_key: chain_key.into(),
            metadata,
            thought_count,
            head_hash,
            generated_at,
            integrity: VectorSidecarIntegrity::sha256(0, String::new()),
            entries,
            wal_base_digest_hex: None,
            store: VectorStore::default(),
        };
        sidecar.rehome();
        sidecar.validate_entries()?;
        sidecar.integrity = sidecar.compute_integrity()?;
        Ok(sidecar)
    }

    /// Load, validate, and integrity-check a sidecar from disk.
    ///
    /// If a sibling `.wal` file exists, incremental records are replayed and
    /// verified against the snapshot digest before being applied.
    ///
    /// A background compaction ([`compact_sidecar_snapshot`]) may swap the
    /// snapshot and delete the WAL while this runs; reading the old snapshot and
    /// then finding no WAL would silently drop every record the WAL held. The
    /// load therefore notices that the snapshot file changed under it and reads
    /// again (up to three attempts).
    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        Self::load_from_path_with_hook(path, &mut || {})
    }

    /// [`Self::load_from_path`] with a hook that runs after the snapshot is read
    /// and before the WAL is: the point a concurrent compaction can tear a load.
    #[doc(hidden)]
    pub fn load_from_path_with_hook(
        path: &Path,
        after_snapshot: &mut dyn FnMut(),
    ) -> io::Result<Self> {
        const ATTEMPTS: usize = 3;
        let mut attempt = 1;
        loop {
            let before = file_stamp(path)?;
            let mut sidecar = Self::read_snapshot(path)?;
            after_snapshot();
            replay_sidecar_wal(&mut sidecar, &sidecar_wal_path(path))?;
            let settled = file_stamp(path).is_ok_and(|after| after == before);
            if settled || attempt == ATTEMPTS {
                return Ok(sidecar);
            }
            attempt += 1;
        }
    }

    /// Move every entry's vector into this sidecar's own flat store, one entry
    /// at a time so the loose copies are freed as the store fills (peak memory
    /// stays near one copy plus a chunk). Entries of the wrong dimension are
    /// left alone for validation to reject.
    fn rehome(&mut self) {
        let dimension = self.metadata.dimension;
        self.store = VectorStore::new(dimension);
        for entry in &mut self.entries {
            // Rows a producer already packed (a rebuild) stay where they are:
            // copying them would hold the corpus twice.
            if entry.vector.len() == dimension && !entry.vector.is_packed() {
                entry.vector = self.store.adopt(&entry.vector);
            }
        }
    }

    /// Append `entry`, moving its vector into the store first.
    fn push_entry(&mut self, mut entry: VectorSidecarEntry) {
        let dimension = self.metadata.dimension;
        if entry.vector.len() == dimension && !entry.vector.is_packed() {
            if !self.store.has_dimension(dimension) {
                self.store = VectorStore::new(dimension);
            }
            entry.vector = self.store.adopt(&entry.vector);
        }
        self.entries.push(entry);
    }

    /// Read and verify the JSON snapshot alone, leaving the WAL unreplayed.
    ///
    /// The returned sidecar's `integrity` is already the WAL chain head (see
    /// [`VectorSidecar::wal_base_digest_hex`]), ready for replay.
    fn read_snapshot(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut sidecar: Self = serde_json::from_reader(reader).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize vector sidecar: {error}"),
            )
        })?;
        sidecar.rehome();
        sidecar.verify_integrity()?;
        if let Some(base) = sidecar.wal_base_digest_hex.take() {
            sidecar.integrity.digest_hex = base;
        }
        Ok(sidecar)
    }

    /// Append one embedding to this in-memory sidecar and return the WAL record.
    ///
    /// Integrity is chained as `SHA-256(prev_digest || entry bytes)` so a later
    /// compact can rewrite the JSON snapshot without hashing the whole corpus
    /// on every append. After [`VectorSidecar::compact_to_path`] the chain head
    /// is the snapshot digest, which that call adopts; a background compaction
    /// ([`compact_sidecar_snapshot`]) instead leaves the live chain head alone
    /// and records it in the snapshot as
    /// [`VectorSidecar::wal_base_digest_hex`].
    pub fn extend_with_entry(
        &mut self,
        entry: VectorSidecarEntry,
        thought_count: usize,
        head_hash: Option<String>,
    ) -> io::Result<VectorSidecarWalRecord> {
        if entry.vector.len() != self.metadata.dimension {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "WAL vector dimension {} does not match sidecar {}",
                    entry.vector.len(),
                    self.metadata.dimension
                ),
            ));
        }
        let prev_digest_hex = self.integrity.digest_hex.clone();
        let digest_hex = incremental_sidecar_digest(
            &prev_digest_hex,
            &entry,
            thought_count,
            head_hash.as_deref(),
        );
        let record = VectorSidecarWalRecord {
            thought_id: entry.thought_id,
            thought_index: entry.thought_index,
            thought_hash: entry.thought_hash.clone(),
            vector: entry.vector.to_vec(),
            thought_count,
            head_hash: head_hash.clone(),
            prev_digest_hex,
            digest_hex: digest_hex.clone(),
        };
        self.push_entry(entry);
        self.thought_count = thought_count;
        self.head_hash = head_hash;
        self.generated_at = Utc::now();
        self.integrity = VectorSidecarIntegrity::sha256(self.entries.len(), digest_hex);
        Ok(record)
    }

    /// Rewrite the JSON snapshot, clear the WAL, and adopt the persisted
    /// snapshot digest as the WAL chain head.
    ///
    /// Compaction collapses `snapshot + WAL` into a single snapshot whose stored
    /// digest is the full-corpus digest. `self.integrity` is advanced to that
    /// same digest so a later [`VectorSidecar::extend_with_entry`] chains from
    /// the digest actually on disk rather than the pre-compaction incremental
    /// digest, which would fail WAL replay on the next load.
    pub fn compact_to_path(&mut self, path: &Path) -> io::Result<()> {
        self.refresh_snapshot_integrity()?;
        self.save_to_path(path)
    }

    /// Recompute `integrity` as the full-corpus snapshot digest.
    ///
    /// Incremental appends keep `integrity` linked to the previous WAL record so
    /// the next record can chain to it without rehashing the corpus.
    /// [`VectorSidecar::compact_to_path`] replaces that incremental chain with
    /// the full-corpus digest stored in the snapshot and adopts it here before
    /// the handle writes another WAL record.
    ///
    /// This is `O(entries x dimension)`; never call it under a chain lock. The
    /// managed append path does not: it leaves compaction to
    /// [`spawn_sidecar_compaction`].
    pub(crate) fn refresh_snapshot_integrity(&mut self) -> io::Result<()> {
        self.integrity = self.compute_integrity()?;
        Ok(())
    }

    /// Persist a full sidecar snapshot to disk, clearing any sibling WAL.
    ///
    /// A complete snapshot supersedes incremental WAL records, so the sibling
    /// `.wal` is removed before the snapshot is atomically replaced. Clearing it
    /// first keeps the on-disk pair recoverable: a crash can leave the previous
    /// snapshot without a WAL (merely stale, so it rebuilds) but never a newer
    /// snapshot paired with an older WAL, which is the state that fails digest
    /// replay.
    ///
    /// Integrity is checked on load. Skipping a second serialize+hash here keeps
    /// append-time writes from paying O(n) hashing twice.
    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            crate::private_fs::create_dir_all(parent)?;
        }
        let temp_path = sidecar_temp_path(path);
        let file = crate::private_fs::create(&temp_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, self).map_err(|error| {
            io::Error::other(format!("Failed to serialize vector sidecar: {error}"))
        })?;
        writer.flush()?;
        remove_sidecar_wal(path)?;
        replace_sidecar_file(&temp_path, path)
    }

    /// Recompute and verify the sidecar's integrity metadata.
    pub fn verify_integrity(&self) -> io::Result<()> {
        self.validate_entries()?;
        if self.schema_version != VECTOR_SIDECAR_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported vector sidecar schema version {}",
                    self.schema_version
                ),
            ));
        }
        if self.integrity.algorithm != "sha256" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported vector sidecar integrity algorithm '{}'",
                    self.integrity.algorithm
                ),
            ));
        }
        if self.integrity.entry_count != self.entries.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Vector sidecar integrity entry count mismatch: expected {}, got {}",
                    self.integrity.entry_count,
                    self.entries.len()
                ),
            ));
        }
        let expected = self.compute_integrity()?;
        if self.integrity != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Vector sidecar integrity digest mismatch",
            ));
        }
        Ok(())
    }

    /// Compare this sidecar with the current chain and embedding-space metadata.
    pub fn freshness(
        &self,
        chain_key: &str,
        thought_count: usize,
        head_hash: Option<&str>,
        metadata: &EmbeddingMetadata,
    ) -> VectorSidecarFreshness {
        if self.chain_key != chain_key {
            return VectorSidecarFreshness::ChainKeyMismatch {
                expected: chain_key.to_string(),
                actual: self.chain_key.clone(),
            };
        }
        if self.metadata.model_id != metadata.model_id {
            return VectorSidecarFreshness::ModelMismatch {
                expected: metadata.model_id.clone(),
                actual: self.metadata.model_id.clone(),
            };
        }
        if self.metadata.embedding_version != metadata.embedding_version {
            return VectorSidecarFreshness::EmbeddingVersionMismatch {
                expected: metadata.embedding_version.clone(),
                actual: self.metadata.embedding_version.clone(),
            };
        }
        if self.metadata.dimension != metadata.dimension {
            return VectorSidecarFreshness::DimensionMismatch {
                expected: metadata.dimension,
                actual: self.metadata.dimension,
            };
        }
        if self.thought_count != thought_count {
            return VectorSidecarFreshness::StaleThoughtCount {
                expected: thought_count,
                actual: self.thought_count,
            };
        }
        if self.entries.len() != self.thought_count {
            // Coverage self-check: count and head hash can both match while
            // entries are missing (a lost WAL record, a hand-edited file), so a
            // gap must also read as stale or the thought stays vector-invisible.
            return VectorSidecarFreshness::StaleThoughtCount {
                expected: thought_count,
                actual: self.entries.len(),
            };
        }
        let expected_head_hash = head_hash.map(str::to_string);
        if self.head_hash != expected_head_hash {
            return VectorSidecarFreshness::StaleHeadHash {
                expected: expected_head_hash,
                actual: self.head_hash.clone(),
            };
        }
        VectorSidecarFreshness::Fresh
    }

    fn validate_entries(&self) -> io::Result<()> {
        let mut thought_ids = HashSet::new();
        let mut thought_hashes = HashSet::new();
        let mut previous_index = None;
        for entry in &self.entries {
            if entry.vector.len() != self.metadata.dimension {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Vector sidecar entry for thought {} has dimension {}, expected {}",
                        entry.thought_id,
                        entry.vector.len(),
                        self.metadata.dimension,
                    ),
                ));
            }
            for (value_index, value) in entry.vector.iter().enumerate() {
                if !value.is_finite() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Vector sidecar entry for thought {} contains non-finite value at index {}",
                            entry.thought_id, value_index
                        ),
                    ));
                }
            }
            if !thought_ids.insert(entry.thought_id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Duplicate vector sidecar thought id {}", entry.thought_id),
                ));
            }
            if !thought_hashes.insert(entry.thought_hash.clone()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Duplicate vector sidecar thought hash {}",
                        entry.thought_hash
                    ),
                ));
            }
            if let Some(previous_index) = previous_index {
                if entry.thought_index < previous_index {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Vector sidecar entries must be ordered by thought_index",
                    ));
                }
            }
            previous_index = Some(entry.thought_index);
        }
        Ok(())
    }

    fn compute_integrity(&self) -> io::Result<VectorSidecarIntegrity> {
        FULL_DIGESTS_ON_THIS_THREAD.with(|count| count.set(count.get() + 1));
        #[derive(Serialize)]
        struct DigestPayload<'a> {
            schema_version: u32,
            chain_key: &'a str,
            metadata: &'a EmbeddingMetadata,
            thought_count: usize,
            head_hash: &'a Option<String>,
            generated_at: DateTime<Utc>,
            entries: &'a [VectorSidecarEntry],
        }

        let payload = DigestPayload {
            schema_version: self.schema_version,
            chain_key: &self.chain_key,
            metadata: &self.metadata,
            thought_count: self.thought_count,
            head_hash: &self.head_hash,
            generated_at: self.generated_at,
            entries: &self.entries,
        };
        // Stream the canonical JSON straight into the hasher: materializing it
        // first allocated the whole corpus (hundreds of MiB for a large chain).
        let mut hasher = HashingWriter(Sha256::new());
        serde_json::to_writer(&mut hasher, &payload).map_err(|error| {
            io::Error::other(format!(
                "Failed to serialize vector sidecar integrity payload: {error}"
            ))
        })?;
        let hasher = hasher.0;
        Ok(VectorSidecarIntegrity::sha256(
            self.entries.len(),
            hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        ))
    }
}

/// `io::Write` adapter that feeds every byte written into a SHA-256 hasher.
struct HashingWriter(Sha256);

impl Write for HashingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One incremental sidecar append record.
///
/// Chained to the previous digest so a truncated or reordered WAL fails
/// verification on load.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VectorSidecarWalRecord {
    /// Thought UUID for this embedding.
    pub thought_id: Uuid,
    /// Append-order index of the thought.
    pub thought_index: u64,
    /// Thought content hash.
    pub thought_hash: String,
    /// Embedding vector.
    pub vector: Vec<f32>,
    /// Chain thought count after this append.
    pub thought_count: usize,
    /// Chain head hash after this append.
    pub head_hash: Option<String>,
    /// Sidecar digest before this record.
    pub prev_digest_hex: String,
    /// Sidecar digest after this record.
    pub digest_hex: String,
}

/// WAL path beside a JSON sidecar snapshot (`foo.json` → `foo.json.wal`).
///
/// The WAL is not a second source of truth: it is only applied after the
/// snapshot digest verifies. A truncated or reordered file fails replay.
pub fn sidecar_wal_path(snapshot_path: &Path) -> PathBuf {
    let mut os = snapshot_path.as_os_str().to_os_string();
    os.push(".wal");
    PathBuf::from(os)
}

/// Remove the sibling WAL for `snapshot_path` if it exists.
///
/// A missing WAL is not an error: most snapshots are written without one.
fn remove_sidecar_wal(snapshot_path: &Path) -> io::Result<()> {
    match fs::remove_file(sidecar_wal_path(snapshot_path)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Append one verified WAL record to `wal_path`.
///
/// Writes magic `MDBVWAL1` on a new file, then `u32` little-endian payload
/// length plus a bincode `VectorSidecarWalRecord`. Callers must have already
/// chained `prev_digest_hex` / `digest_hex` via
/// [`VectorSidecar::extend_with_entry`].
pub fn append_sidecar_wal_record(
    wal_path: &Path,
    record: &VectorSidecarWalRecord,
) -> io::Result<()> {
    if let Some(parent) = wal_path.parent() {
        crate::private_fs::create_dir_all(parent)?;
    }
    let mut file = crate::private_fs::options()
        .create(true)
        .append(true)
        .open(wal_path)?;
    if file.metadata()?.len() == 0 {
        file.write_all(WAL_MAGIC)?;
    }
    let payload =
        bincode::serde::encode_to_vec(record, bincode::config::standard()).map_err(|error| {
            io::Error::other(format!("Failed to encode sidecar WAL record: {error}"))
        })?;
    file.write_all(&(payload.len() as u32).to_le_bytes())?;
    file.write_all(&payload)?;
    file.flush()?;
    Ok(())
}

fn incremental_sidecar_digest(
    prev_digest_hex: &str,
    entry: &VectorSidecarEntry,
    thought_count: usize,
    head_hash: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_digest_hex.as_bytes());
    hasher.update(entry.thought_id.as_bytes());
    hasher.update(entry.thought_index.to_le_bytes());
    hasher.update(entry.thought_hash.as_bytes());
    hasher.update(thought_count.to_le_bytes());
    if let Some(head) = head_hash {
        hasher.update(head.as_bytes());
    }
    for value in entry.vector.iter() {
        hasher.update(value.to_le_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// Reject a WAL record that would leave `sidecar` with a row
/// [`VectorSidecar::verify_integrity`] would refuse: wrong dimension, a
/// non-finite component, or an index before the previous row's.
fn check_wal_entry(sidecar: &VectorSidecar, entry: &VectorSidecarEntry) -> io::Result<()> {
    if entry.vector.len() != sidecar.metadata.dimension {
        return Err(wal_invalid(format!(
            "vector sidecar WAL record has dimension {}, expected {}",
            entry.vector.len(),
            sidecar.metadata.dimension
        )));
    }
    if entry.vector.iter().any(|value| !value.is_finite()) {
        return Err(wal_invalid(
            "vector sidecar WAL record has a non-finite component",
        ));
    }
    if sidecar
        .entries
        .last()
        .is_some_and(|last| entry.thought_index < last.thought_index)
    {
        return Err(wal_invalid(
            "vector sidecar WAL record is out of thought_index order",
        ));
    }
    Ok(())
}

/// What replaying one WAL byte stream did to a sidecar.
struct WalReplay {
    /// Records chained onto the sidecar.
    applied: usize,
    /// Leading records the snapshot already covered (a compaction that
    /// swapped the snapshot but not yet the WAL).
    skipped: usize,
    /// Offset just past the last whole record. Shorter than the input when the
    /// stream ends in a torn length prefix.
    complete_len: usize,
}

fn replay_sidecar_wal(sidecar: &mut VectorSidecar, wal_path: &Path) -> io::Result<()> {
    let bytes = match fs::read(wal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    replay_sidecar_wal_bytes(sidecar, &bytes).map(|_| ())
}

fn wal_invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Replay the WAL stream `bytes` onto `sidecar`, whose `integrity` must hold
/// the digest the first record chains from.
///
/// Leading records the sidecar already covers are skipped when they form a
/// verified chain that ends at the sidecar's WAL head: that is the state a
/// crash leaves between the two file swaps of [`compact_sidecar_snapshot`].
/// Any other digest mismatch is an error.
fn replay_sidecar_wal_bytes(sidecar: &mut VectorSidecar, bytes: &[u8]) -> io::Result<WalReplay> {
    let mut replay = WalReplay {
        applied: 0,
        skipped: 0,
        complete_len: 0,
    };
    // Fewer than 8 bytes is a WAL that never got its magic: nothing to replay.
    let Some(magic) = bytes.get(..WAL_MAGIC.len()) else {
        return Ok(replay);
    };
    if magic != WAL_MAGIC {
        return Err(wal_invalid(
            "vector sidecar WAL has an unknown magic header",
        ));
    }
    let mut offset = WAL_MAGIC.len();
    replay.complete_len = offset;
    let mut last_skipped: Option<String> = None;
    while let Some(len_bytes) = bytes.get(offset..offset + 4) {
        let len = u32::from_le_bytes(len_bytes.try_into().expect("slice of four bytes")) as usize;
        if len == 0 || len > 16 * 1024 * 1024 {
            return Err(wal_invalid(format!(
                "vector sidecar WAL record length {len} is invalid"
            )));
        }
        let payload_start = offset + 4;
        let Some(payload) = bytes.get(payload_start..payload_start + len) else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "vector sidecar WAL ends inside a record",
            ));
        };
        let (record, _): (VectorSidecarWalRecord, _) =
            bincode::serde::decode_from_slice(payload, crate::decode_limits::file_config())
                .map_err(|error| {
                    wal_invalid(format!("Failed to decode sidecar WAL record: {error}"))
                })?;
        offset = payload_start + len;
        replay.complete_len = offset;
        let entry = VectorSidecarEntry::new(
            record.thought_id,
            record.thought_index,
            record.thought_hash,
            record.vector,
        );
        let expected = incremental_sidecar_digest(
            &record.prev_digest_hex,
            &entry,
            record.thought_count,
            record.head_hash.as_deref(),
        );
        if record.prev_digest_hex != sidecar.integrity.digest_hex {
            let covered = replay.applied == 0
                && (entry.thought_index as usize) < sidecar.entries.len()
                && last_skipped
                    .as_ref()
                    .is_none_or(|last| *last == record.prev_digest_hex)
                && expected == record.digest_hex;
            if !covered {
                return Err(wal_invalid("vector sidecar WAL digest chain mismatch"));
            }
            last_skipped = Some(record.digest_hex);
            replay.skipped += 1;
            continue;
        }
        if expected != record.digest_hex {
            return Err(wal_invalid("vector sidecar WAL record digest mismatch"));
        }
        // The first record after the skipped ones must start exactly where the
        // last skipped one ended.
        if replay.applied == 0
            && last_skipped
                .as_ref()
                .is_some_and(|last| *last != record.prev_digest_hex)
        {
            return Err(wal_invalid("vector sidecar WAL digest chain mismatch"));
        }
        // The digest chain proves the record follows the previous one, not that
        // it is well formed: it is a checksum, not an authenticator.
        check_wal_entry(sidecar, &entry)?;
        sidecar.push_entry(entry);
        sidecar.thought_count = record.thought_count;
        sidecar.head_hash = record.head_hash;
        sidecar.integrity =
            VectorSidecarIntegrity::sha256(sidecar.entries.len(), record.digest_hex);
        replay.applied += 1;
    }
    if replay.applied == 0
        && replay.skipped > 0
        && last_skipped != Some(sidecar.integrity.digest_hex.clone())
    {
        // Every record was skipped, so the last one must be exactly where the
        // snapshot's WAL chain starts; otherwise this WAL belongs to some
        // other snapshot.
        return Err(wal_invalid("vector sidecar WAL digest chain mismatch"));
    }
    Ok(replay)
}

thread_local! {
    /// Full-corpus digests computed by the current thread.
    static FULL_DIGESTS_ON_THIS_THREAD: Cell<usize> = const { Cell::new(0) };
}

/// Number of full-corpus sidecar digests (`O(entries x dimension)`) the calling
/// thread has computed so far.
///
/// A test hook, not an API to build on: it lets a test prove that a code path
/// that must stay cheap (anything under a chain write lock) never hashes the
/// whole corpus, without timing it. The count is per thread, so a compaction
/// running on its own thread is not charged to the caller.
#[doc(hidden)]
pub fn full_sidecar_digests_on_this_thread() -> usize {
    FULL_DIGESTS_ON_THIS_THREAD.with(Cell::get)
}

/// A point inside [`compact_sidecar_snapshot`] where a test can observe or
/// interrupt the compaction.
///
/// Returning an error from the hook of [`compact_sidecar_snapshot_with_hook`]
/// stops the compaction at that point with the files exactly as they are, which
/// is what a crash there would leave.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarCompactionStage {
    /// The snapshot and the WAL records present so far are folded into the new
    /// snapshot, which sits in its temporary file. No lock is held. A WAL
    /// record appended now lands in the retained tail.
    SnapshotWritten,
    /// The new snapshot replaced the old one; the old, longer WAL is still in
    /// place.
    SnapshotSwapped,
}

/// What one [`compact_sidecar_snapshot`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarCompactionReport {
    /// Entries in the new snapshot.
    pub entries: usize,
    /// WAL records folded into the snapshot and dropped from the WAL.
    pub folded_records: usize,
    /// WAL records appended while the compaction ran, kept in the shortened
    /// WAL.
    pub retained_records: usize,
}

/// Modification time and length of `path`, to notice another writer replacing
/// the file mid-compaction.
fn file_stamp(path: &Path) -> io::Result<(u64, Option<std::time::SystemTime>)> {
    let metadata = fs::metadata(path)?;
    Ok((metadata.len(), metadata.modified().ok()))
}

fn read_wal_bytes(wal_path: &Path) -> io::Result<Vec<u8>> {
    match fs::read(wal_path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Fold the sidecar WAL into the JSON snapshot at `snapshot_path`, without
/// touching any in-memory sidecar and without blocking appends.
///
/// This is the compaction a managed chain requests every
/// [`wal_compact_threshold`] appends (see [`spawn_sidecar_compaction`]). It
/// works from the files on disk:
///
/// 1. read and verify the snapshot (the slow, `O(entries x dimension)` part;
///    no lock);
/// 2. under `io_lock`, copy the WAL bytes;
/// 3. replay them, hash the whole corpus and write the new snapshot to a
///    temporary file (slow; no lock). Appends that land now are not lost: the
///    WAL keeps growing;
/// 4. under `io_lock`, check that the snapshot was not replaced meanwhile,
///    swap the snapshot in, then replace the WAL with only the records
///    appended since step 2.
///
/// `io_lock` must be the lock every writer of this sidecar's WAL holds while it
/// appends (a managed chain's `flush_pending_derived` does), so that steps 2
/// and 4 never observe a half-written record. It is held only for file reads,
/// small writes and renames: milliseconds, independent of the corpus size.
///
/// Returns `Ok(None)` without changing anything when there is nothing to do
/// (no snapshot) or the snapshot changed under the compaction (for example a
/// full rebuild was adopted); the next request will try again.
///
/// # Errors
///
/// Fails when the snapshot or the WAL is unreadable, corrupt or ends in a torn
/// record, or on an I/O error. The files are left as they were or, after a
/// failure between the two swaps, in a state [`VectorSidecar::load_from_path`]
/// recovers.
///
/// # Examples
///
/// ```
/// use mentisdb::search::{
///     append_sidecar_wal_record, compact_sidecar_snapshot, sidecar_wal_path,
///     EmbeddingMetadata, VectorSidecar, VectorSidecarEntry,
/// };
/// use std::sync::Mutex;
/// use uuid::Uuid;
///
/// # fn main() -> std::io::Result<()> {
/// let dir = tempfile::tempdir()?;
/// let path = dir.path().join("chain.vectors.json");
/// let mut sidecar = VectorSidecar::build(
///     "demo",
///     EmbeddingMetadata::new("model", 2, "v1"),
///     0,
///     None,
///     chrono::Utc::now(),
///     Vec::new(),
/// )?;
/// sidecar.save_to_path(&path)?;
/// for index in 0..3_u64 {
///     let entry = VectorSidecarEntry::new(
///         Uuid::from_u128(index as u128 + 1),
///         index,
///         format!("hash-{index}"),
///         vec![1.0, index as f32],
///     );
///     let record = sidecar.extend_with_entry(
///         entry,
///         index as usize + 1,
///         Some(format!("hash-{index}")),
///     )?;
///     append_sidecar_wal_record(&sidecar_wal_path(&path), &record)?;
/// }
///
/// let report = compact_sidecar_snapshot(&path, &Mutex::new(()))?.expect("compacted");
/// assert_eq!(report.folded_records, 3);
/// assert!(!sidecar_wal_path(&path).exists());
/// assert_eq!(VectorSidecar::load_from_path(&path)?.entries.len(), 3);
/// # Ok(())
/// # }
/// ```
pub fn compact_sidecar_snapshot(
    snapshot_path: &Path,
    io_lock: &Mutex<()>,
) -> io::Result<Option<SidecarCompactionReport>> {
    compact_sidecar_snapshot_with_hook(snapshot_path, io_lock, &mut |_| Ok(()))
}

/// [`compact_sidecar_snapshot`] with a hook called at each
/// [`SidecarCompactionStage`]; an error from the hook aborts the compaction
/// there. Exists for crash-recovery and concurrency tests.
#[doc(hidden)]
pub fn compact_sidecar_snapshot_with_hook(
    snapshot_path: &Path,
    io_lock: &Mutex<()>,
    hook: &mut dyn FnMut(SidecarCompactionStage) -> io::Result<()>,
) -> io::Result<Option<SidecarCompactionReport>> {
    let lock = || io_lock.lock().unwrap_or_else(|error| error.into_inner());
    let wal_path = sidecar_wal_path(snapshot_path);
    let stamp = match file_stamp(snapshot_path) {
        Ok(stamp) => stamp,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Slow, lock-free: parse and verify the snapshot.
    let mut merged = VectorSidecar::read_snapshot(snapshot_path)?;
    // Brief: copy the WAL as it stands.
    let folded_bytes = {
        let _guard = lock();
        read_wal_bytes(&wal_path)?
    };
    // Slow, lock-free: replay, hash, write.
    let replay = replay_sidecar_wal_bytes(&mut merged, &folded_bytes)?;
    if replay.complete_len != folded_bytes.len() {
        return Err(wal_invalid(
            "vector sidecar WAL ends in a torn record; not compacting",
        ));
    }
    let chain_head = merged.integrity.digest_hex.clone();
    merged.integrity = merged.compute_integrity()?;
    merged.wal_base_digest_hex = Some(chain_head);
    let temp_path = compact_temp_path(snapshot_path);
    write_snapshot_file(&merged, &temp_path)?;
    hook(SidecarCompactionStage::SnapshotWritten)?;
    // Brief: swap the files.
    let retained = {
        let _guard = lock();
        let unchanged = file_stamp(snapshot_path).is_ok_and(|now| now == stamp);
        let current = read_wal_bytes(&wal_path)?;
        if !unchanged || !current.starts_with(&folded_bytes) {
            let _ = fs::remove_file(&temp_path);
            return Ok(None);
        }
        replace_sidecar_file(&temp_path, snapshot_path)?;
        hook(SidecarCompactionStage::SnapshotSwapped)?;
        let tail = &current[folded_bytes.len()..];
        swap_wal_tail(&wal_path, &folded_bytes, tail)?;
        count_wal_records(tail)
    };
    Ok(Some(SidecarCompactionReport {
        entries: merged.entries.len(),
        folded_records: replay.applied + replay.skipped,
        retained_records: retained,
    }))
}

/// Number of whole length-prefixed records in a WAL tail (no magic header).
fn count_wal_records(mut tail: &[u8]) -> usize {
    let mut count = 0;
    while let Some(len_bytes) = tail.get(..4) {
        let len = u32::from_le_bytes(len_bytes.try_into().expect("slice of four bytes")) as usize;
        let Some(rest) = tail.get(4 + len..) else {
            break;
        };
        tail = rest;
        count += 1;
    }
    count
}

/// Replace the WAL with its magic header plus `tail`, or delete it when no
/// record is left.
fn swap_wal_tail(wal_path: &Path, folded: &[u8], tail: &[u8]) -> io::Result<()> {
    if tail.is_empty() {
        return match fs::remove_file(wal_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }
    let temp_path = compact_temp_path(wal_path);
    let mut file = crate::private_fs::create(&temp_path)?;
    // The magic is the first eight bytes of the folded stream, which is
    // non-empty whenever a tail follows it.
    file.write_all(folded.get(..WAL_MAGIC.len()).unwrap_or(WAL_MAGIC))?;
    file.write_all(tail)?;
    file.sync_all()?;
    replace_sidecar_file(&temp_path, wal_path)
}

fn compact_temp_path(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".compact.tmp");
    PathBuf::from(os)
}

/// Write `sidecar` as pretty-compact JSON to `path` and flush it to disk.
fn write_snapshot_file(sidecar: &VectorSidecar, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        crate::private_fs::create_dir_all(parent)?;
    }
    let file = crate::private_fs::create(path)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, sidecar).map_err(|error| {
        io::Error::other(format!("Failed to serialize vector sidecar: {error}"))
    })?;
    writer.flush()?;
    writer.get_ref().sync_all()
}

/// Sidecar snapshot paths with a compaction queued or running.
static COMPACTIONS_IN_FLIGHT: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
/// Signalled whenever a compaction finishes.
static COMPACTION_FINISHED: Condvar = Condvar::new();

/// Removes a path from [`COMPACTIONS_IN_FLIGHT`] when dropped, including when
/// the compaction panics.
struct CompactionToken(PathBuf);

impl Drop for CompactionToken {
    fn drop(&mut self) {
        let mut in_flight = COMPACTIONS_IN_FLIGHT
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(set) = in_flight.as_mut() {
            set.remove(&self.0);
        }
        COMPACTION_FINISHED.notify_all();
    }
}

/// Run [`compact_sidecar_snapshot`] for `snapshot_path` on a new thread.
///
/// Single-flight per snapshot path: returns `false` without starting anything
/// when a compaction of the same file is already running (the running one folds
/// the WAL as of its own start; the next request after it picks up the rest).
/// A failure is logged to stderr and is otherwise harmless: the sidecar is
/// derived state and the WAL keeps it complete.
///
/// `io_lock` is the lock the WAL writers hold while appending; see
/// [`compact_sidecar_snapshot`].
///
/// Also returns `false` when the thread cannot be spawned.
pub fn spawn_sidecar_compaction(snapshot_path: PathBuf, io_lock: Arc<Mutex<()>>) -> bool {
    {
        let mut in_flight = COMPACTIONS_IN_FLIGHT
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !in_flight
            .get_or_insert_with(HashSet::new)
            .insert(snapshot_path.clone())
        {
            return false;
        }
    }
    let token = CompactionToken(snapshot_path.clone());
    let spawned = std::thread::Builder::new()
        .name("mentisdb-sidecar-compact".to_string())
        .spawn(move || {
            let _token = token;
            if let Err(error) = compact_sidecar_snapshot(&snapshot_path, &io_lock) {
                eprintln!(
                    "[mentisdb] vector sidecar compaction of {} failed: {error}",
                    snapshot_path.display()
                );
            }
        });
    spawned.is_ok()
}

/// Block until no compaction of `snapshot_path` is queued or running.
///
/// A test hook so a test can observe the files after the background work.
#[doc(hidden)]
pub fn wait_for_sidecar_compaction(snapshot_path: &Path) {
    let mut in_flight = COMPACTIONS_IN_FLIGHT
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    while in_flight
        .as_ref()
        .is_some_and(|set| set.contains(snapshot_path))
    {
        in_flight = COMPACTION_FINISHED
            .wait(in_flight)
            .unwrap_or_else(|error| error.into_inner());
    }
}

fn sidecar_temp_path(path: &Path) -> std::path::PathBuf {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!("{value}.tmp"))
        .unwrap_or_else(|| "tmp".to_string());
    path.with_extension(extension)
}

fn replace_sidecar_file(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if target.exists() {
        fs::remove_file(target)?;
    }
    fs::rename(source, target)
}
