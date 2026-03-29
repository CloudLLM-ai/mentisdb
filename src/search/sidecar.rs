//! Rebuildable vector sidecar persistence for one durable chain.
//!
//! Vector sidecars are derived artifacts keyed by one chain and one embedding
//! space. They never replace the append-only chain itself, and they can always
//! be rebuilt from the canonical thought log.

use crate::search::EmbeddingMetadata;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::Path;
use uuid::Uuid;

/// Current schema version for persisted vector sidecars.
pub const VECTOR_SIDECAR_SCHEMA_VERSION: u32 = 1;

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
    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let sidecar: Self = serde_json::from_reader(reader).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to deserialize vector sidecar: {error}"),
            )
        })?;
        sidecar.verify_integrity()?;
        Ok(sidecar)
    }

    /// Persist a validated sidecar to disk.
    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        self.verify_integrity()?;
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
