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
//! [`VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD`] records (32),
//! [`VectorSidecar::compact_to_path`] rewrites the JSON snapshot and deletes
//! the WAL. Binaries that only understand the JSON snapshot see a stale
//! sidecar when a WAL is pending and rebuild from the chain.

use crate::search::EmbeddingMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Current schema version for persisted vector sidecars.
pub const VECTOR_SIDECAR_SCHEMA_VERSION: u32 = 1;
/// Compact the JSON snapshot after this many incremental WAL records.
pub const VECTOR_SIDECAR_WAL_COMPACT_THRESHOLD: usize = 32;
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
    pub vector: Vec<f32>,
}

impl VectorSidecarEntry {
    /// Create one persisted vector row.
    pub fn new(
        thought_id: Uuid,
        thought_index: u64,
        thought_hash: impl Into<String>,
        vector: Vec<f32>,
    ) -> Self {
        Self {
            thought_id,
            thought_index,
            thought_hash: thought_hash.into(),
            vector,
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
        };
        sidecar.validate_entries()?;
        sidecar.integrity = sidecar.compute_integrity()?;
        Ok(sidecar)
    }

    /// Load, validate, and integrity-check a sidecar from disk.
    ///
    /// If a sibling `.wal` file exists, incremental records are replayed and
    /// verified against the snapshot digest before being applied.
    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut sidecar: Self = serde_json::from_reader(reader).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize vector sidecar: {error}"),
            )
        })?;
        sidecar.verify_integrity()?;
        replay_sidecar_wal(&mut sidecar, &sidecar_wal_path(path))?;
        Ok(sidecar)
    }

    /// Append one embedding to this in-memory sidecar and return the WAL record.
    ///
    /// Integrity is chained as `SHA-256(prev_digest || entry bytes)` so a later
    /// compact can rewrite the JSON snapshot without hashing the whole corpus
    /// on every append.
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
            vector: entry.vector.clone(),
            thought_count,
            head_hash: head_hash.clone(),
            prev_digest_hex,
            digest_hex: digest_hex.clone(),
        };
        self.entries.push(entry);
        self.thought_count = thought_count;
        self.head_hash = head_hash;
        self.generated_at = Utc::now();
        self.integrity = VectorSidecarIntegrity::sha256(self.entries.len(), digest_hex);
        Ok(record)
    }

    /// Rewrite the JSON snapshot and delete the WAL so a full verify matches.
    pub fn compact_to_path(&self, path: &Path) -> io::Result<()> {
        let mut snapshot = self.clone();
        snapshot.integrity = snapshot.compute_integrity()?;
        snapshot.save_to_path(path)?;
        let wal = sidecar_wal_path(path);
        if wal.exists() {
            fs::remove_file(wal)?;
        }
        Ok(())
    }

    /// Persist a sidecar to disk.
    ///
    /// Integrity is checked on load. Skipping a second serialize+hash here
    /// keeps append-time writes from paying O(n) hashing twice.
    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = sidecar_temp_path(path);
        let file = File::create(&temp_path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer(writer, self).map_err(|error| {
            io::Error::other(format!("Failed to serialize vector sidecar: {error}"))
        })?;
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
        let serialized = serde_json::to_vec(&payload).map_err(|error| {
            io::Error::other(format!(
                "Failed to serialize vector sidecar integrity payload: {error}"
            ))
        })?;
        let mut hasher = Sha256::new();
        hasher.update(serialized);
        Ok(VectorSidecarIntegrity::sha256(
            self.entries.len(),
            format!("{:x}", hasher.finalize()),
        ))
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
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
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
    for value in &entry.vector {
        hasher.update(value.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn replay_sidecar_wal(sidecar: &mut VectorSidecar, wal_path: &Path) -> io::Result<()> {
    if !wal_path.exists() {
        return Ok(());
    }
    let mut file = File::open(wal_path)?;
    let mut magic = [0_u8; 8];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(error) => return Err(error),
    }
    if &magic != WAL_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "vector sidecar WAL has an unknown magic header",
        ));
    }
    loop {
        let mut len_bytes = [0_u8; 4];
        match file.read_exact(&mut len_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        }
        let len = u32::from_le_bytes(len_bytes) as usize;
        if len == 0 || len > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("vector sidecar WAL record length {len} is invalid"),
            ));
        }
        let mut payload = vec![0_u8; len];
        file.read_exact(&mut payload)?;
        let (record, _): (VectorSidecarWalRecord, _) =
            bincode::serde::decode_from_slice(&payload, bincode::config::standard()).map_err(
                |error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Failed to decode sidecar WAL record: {error}"),
                    )
                },
            )?;
        if record.prev_digest_hex != sidecar.integrity.digest_hex {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "vector sidecar WAL digest chain mismatch",
            ));
        }
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
        if expected != record.digest_hex {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "vector sidecar WAL record digest mismatch",
            ));
        }
        sidecar.entries.push(entry);
        sidecar.thought_count = record.thought_count;
        sidecar.head_hash = record.head_hash;
        sidecar.integrity =
            VectorSidecarIntegrity::sha256(sidecar.entries.len(), record.digest_hex);
    }
    Ok(())
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
