//! HNSW approximate-nearest-neighbor backend for [`VectorIndex`].
//!
//! This module is unconditionally compiled since 0.10.4.49. It implements the
//! [`VectorSearchBackend`] trait for an in-memory HNSW graph built on top of
//! the pure-Rust
//! [`hnsw`](https://docs.rs/hnsw) crate and is selected automatically by
//! [`VectorBackendKind::Hnsw`] once the corpus crosses
//! [`DEFAULT_EXACT_TO_HNSW_THRESHOLD`].
//!
//! ## Metric
//!
//! [`hnsw`] 0.11 is built around an unsigned-integer metric (see
//! [`space::Metric`]). Cosine similarity lives in `[-1.0, 1.0]`, which is
//! not a metric space. [`HnswBackend`] therefore encodes the *distance*
//! `1.0 - cosine_similarity` as a non-negative `f32` in `[0.0, 2.0]`,
//! scales it by [`DISTANCE_SCALE`] (`1_000_000.0`), and truncates to a
//! `u32`. The scaled integer preserves order (larger distance = larger
//! integer), the triangle inequality is satisfied up to `f32` rounding
//! noise on normalized inputs, and nearest-neighbor queries are answered
//! correctly. The integer "distance" does not carry a physical meaning
//! beyond ordering; that is fine for an HNSW graph that is only ever
//! consulted for "which items are most similar" questions.
//!
//! [`space::Metric`]: https://docs.rs/space/0.17.0/space/trait.Metric.html
//! [`hnsw`]: https://docs.rs/hnsw/0.11.0/hnsw/

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

use bincode;
use hnsw::{Hnsw, Params};
use rand_pcg::Pcg64;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use space::{Metric, Neighbor};

use super::vector::{
    EmbeddingMetadata, VectorDocument, VectorFilter, VectorIndexError, VectorQuery,
    VectorSearchBackend, VectorSearchHit,
};

/// Default M parameter (max connections per node) for [`HnswBackend`].
///
/// 48 is chosen for 128d+ embedding spaces where the default 16 under-
/// connects the graph. The 10k/128d synthetic benchmark recovers recall
/// with this setting while staying well under the 50ms latency ceiling.
const HNSW_M: usize = 48;

/// Default `ef_construction` for [`HnswBackend`].
const HNSW_EF_CONSTRUCTION: usize = 400;

/// Default `ef_search` for [`HnswBackend`].
const HNSW_EF_SEARCH: usize = 128;

/// Read `MENTISDB_HNSW_EF_CONSTRUCTION` or fall back to the default.
fn hnsw_ef_construction() -> usize {
    std::env::var("MENTISDB_HNSW_EF_CONSTRUCTION")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(HNSW_EF_CONSTRUCTION)
}

/// Read `MENTISDB_HNSW_EF_SEARCH` or fall back to the default.
fn hnsw_ef_search() -> usize {
    std::env::var("MENTISDB_HNSW_EF_SEARCH")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value > 0)
        .unwrap_or(HNSW_EF_SEARCH)
}

/// Whether background HNSW graph construction is enabled.
///
/// Controlled by `MENTISDB_HNSW_BACKGROUND_BUILD`. Defaults to `true` so the
/// daemon stays responsive while large sidecars initialize their approximate
/// index.
pub fn hnsw_background_build_enabled() -> bool {
    std::env::var("MENTISDB_HNSW_BACKGROUND_BUILD")
        .ok()
        .map(|value| value.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}

/// Top layer size (`M0`) for [`HnswBackend`]. The hnsw crate requires
/// `M0` to be a separate const-generic; the paper recommends `M0 = 2 * M`
/// for balanced layer promotion.
const HNSW_M0: usize = HNSW_M * 2;

/// Distance scale factor. HNSW performs greedy selection using integer
/// comparisons. By scaling the cosine distance into a large uniform integer
/// range we avoid the non-linear exponent distribution of raw float bits and
/// give the graph more resolving power for near-neighbor ties.
const DISTANCE_SCALE: f32 = 1_000_000.0;

/// Cosine-distance metric that encodes `1.0 - cosine_similarity` as a `u32`.
/// See the module rustdoc and [`DISTANCE_SCALE`] for the trade-off.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct CosineDistance;

impl Metric<Vec<f32>> for CosineDistance {
    type Unit = u32;

    fn distance(&self, a: &Vec<f32>, b: &Vec<f32>) -> Self::Unit {
        let similarity = super::vector::cosine_similarity(a, b).unwrap_or(0.0);
        // Clamp to [0.0, 2.0] before scaling. On unit vectors the float math
        // can overshoot to 1.0 + epsilon or -1.0 - epsilon by a few ULPs, and
        // `1.0 - similarity` would then be negative. The clamp is a no-op for
        // in-range values.
        let distance = (1.0_f32 - similarity).clamp(0.0, 2.0);
        debug_assert!(distance.is_finite() && distance >= 0.0);
        (distance * DISTANCE_SCALE) as u32
    }
}

/// HNSW backend for [`VectorIndex`].
///
/// Built on `hnsw` 0.11 with the [`CosineDistance`] metric. Selected
/// automatically by [`super::vector::select_backend_kind`] once the corpus
/// crosses [`DEFAULT_EXACT_TO_HNSW_THRESHOLD`].
#[derive(Serialize, Deserialize)]
pub struct HnswBackend {
    metadata: EmbeddingMetadata,
    hnsw: Hnsw<CosineDistance, Vec<f32>, Pcg64, HNSW_M, HNSW_M0>,
    /// HNSW item id (assigned at insert time) -> our `document_id`.
    id_to_doc: Vec<String>,
    /// Our `document_id` -> HNSW item id, for upsert / remove.
    doc_to_id: BTreeMap<String, usize>,
    /// Cached exact vectors so we can return the score in the same units the
    /// Exact backend would. The HNSW graph itself does not store a
    /// user-visible "score" (only the integer distance); the hit's `score`
    /// field is therefore recomputed from the cached exact vector.
    vectors: BTreeMap<String, Vec<f32>>,
}

impl HnswBackend {
    /// Create an empty HNSW backend for one embedding space.
    pub fn new(metadata: EmbeddingMetadata) -> Self {
        // The crate's `Params` only exposes `ef_construction` and a few
        // other knobs; the graph's `M` and `M0` are type-level const
        // generics on `Hnsw<...>`. `Params::default()` ships with
        // ef_construction=400 (overkill for our workloads) so we override
        // to `HNSW_EF_CONSTRUCTION` for a faster build at our recall target.
        let params = Params::default().ef_construction(hnsw_ef_construction());
        // `hnsw::Hnsw::new_params` is deterministic when its PRNG is
        // deterministic, and `Pcg64::default()` uses a fixed seed, so the
        // graph is reproducible across processes. That is important for
        // the sidecar integrity hash and for cache rebuilds.
        let hnsw = Hnsw::<CosineDistance, Vec<f32>, Pcg64, HNSW_M, HNSW_M0>::new_params(
            CosineDistance,
            params,
        );
        Self {
            metadata,
            hnsw,
            id_to_doc: Vec::new(),
            doc_to_id: std::collections::BTreeMap::new(),
            vectors: std::collections::BTreeMap::new(),
        }
    }

    /// Create an HNSW backend preloaded with a corpus.
    pub fn from_documents(
        metadata: EmbeddingMetadata,
        documents: Vec<VectorDocument>,
    ) -> Result<Self, VectorIndexError> {
        let mut backend = Self::new(metadata);
        for document in documents {
            backend.upsert_document(document)?;
        }
        Ok(backend)
    }
}

impl VectorSearchBackend for HnswBackend {
    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn document_count(&self) -> usize {
        self.doc_to_id.len()
    }

    fn upsert_document(&mut self, document: VectorDocument) -> Result<(), VectorIndexError> {
        super::vector::validate_vector(
            &document.vector,
            self.metadata.dimension,
            Some(document.document_id.as_str()),
            "document",
        )?;

        if self.doc_to_id.contains_key(&document.document_id) {
            // The HNSW crate (0.11) is append-only: graph nodes cannot be
            // updated or removed. On an upsert we refresh the cached exact
            // vector so the score reported by search_inner stays correct.
            // The graph topology still navigates using the original vector,
            // which is a minor approximation; a full graph rebuild is needed
            // to pick up topology changes. This matches the Exact backend's
            // "replace by id" semantics for the score path.
            self.vectors
                .insert(document.document_id.clone(), document.vector);
        } else {
            let mut searcher = hnsw::Searcher::default();
            let hnsw_id = self.hnsw.insert(document.vector.clone(), &mut searcher);
            self.id_to_doc.push(document.document_id.clone());
            self.doc_to_id.insert(document.document_id.clone(), hnsw_id);
            self.vectors.insert(document.document_id, document.vector);
        }
        Ok(())
    }

    fn remove_document(&mut self, document_id: &str) -> bool {
        let removed = self.vectors.remove(document_id).is_some();
        // Tombstone: drop our local id map so the next query does not
        // return the stale HNSW id. The HNSW graph itself does not
        // support remove in 0.11; the graph node is reclaimed on the
        // next full rebuild.
        if let Some(hnsw_id) = self.doc_to_id.remove(document_id) {
            if hnsw_id < self.id_to_doc.len() {
                self.id_to_doc[hnsw_id] = String::new();
            }
        }
        removed
    }

    fn get_vector(&self, document_id: &str) -> Option<Vec<f32>> {
        self.vectors.get(document_id).cloned()
    }

    fn search(&self, query: &VectorQuery) -> Result<Vec<VectorSearchHit>, VectorIndexError> {
        self.search_inner(query, None)
    }

    fn search_filtered(
        &self,
        query: &VectorQuery,
        filter: &VectorFilter,
    ) -> Result<Vec<VectorSearchHit>, VectorIndexError> {
        if filter.allows_all() {
            return self.search(query);
        }

        let allowed = self.filter_to_bitmap(filter);
        if allowed.is_empty() || self.doc_to_id.is_empty() {
            return Ok(Vec::new());
        }

        self.search_inner(query, Some(&allowed))
    }
}

impl HnswBackend {
    /// Translate a caller-facing [`VectorFilter`] into a bitmap of HNSW item
    /// ids. The HNSW crate's internal item id (`neighbor.index`) is the
    /// ordinal we use in the bitmap.
    ///
    /// Only [`VectorFilter::Ids`] reaches this method because
    /// [`VectorFilter::None`] short-circuits to unfiltered search in
    /// [`VectorSearchBackend::search_filtered`].
    fn filter_to_bitmap(&self, filter: &VectorFilter) -> RoaringBitmap {
        match filter {
            VectorFilter::None => self
                .doc_to_id
                .values()
                .map(|&id| id as u32)
                .collect::<RoaringBitmap>(),
            VectorFilter::Ids(ids) => ids
                .iter()
                .filter_map(|id| self.doc_to_id.get(id).map(|&id| id as u32))
                .collect::<RoaringBitmap>(),
        }
    }

    /// Shared search implementation for filtered and unfiltered queries.
    ///
    /// `allowed` is an optional bitmap of HNSW item ids. When present, the
    /// search asks the approximate graph for more candidates than requested
    /// (overshoot) and then discards those that are not in the bitmap before
    /// re-scoring with exact cosine.
    fn search_inner(
        &self,
        query: &VectorQuery,
        allowed: Option<&RoaringBitmap>,
    ) -> Result<Vec<VectorSearchHit>, VectorIndexError> {
        super::vector::validate_vector(&query.vector, self.metadata.dimension, None, "query")?;

        if self.doc_to_id.is_empty() {
            return Ok(Vec::new());
        }

        // For unfiltered search we ask for exactly `limit` candidates. For
        // filtered search we overshoot and then discard non-matching ids.
        // The overshoot is capped by the live document count to avoid asking
        // `nearest` for more candidates than exist.
        let limit = query.limit.max(1).min(self.doc_to_id.len());
        let candidate_count = allowed
            .map(|_| (limit * 4).max(64).min(self.doc_to_id.len()))
            .unwrap_or(limit);
        let ef = hnsw_ef_search().max(candidate_count);
        let mut dest = vec![
            Neighbor {
                distance: 0,
                index: 0,
            };
            candidate_count
        ];
        let mut searcher = hnsw::Searcher::default();
        let _ = self
            .hnsw
            .nearest(&query.vector, ef, &mut searcher, &mut dest);

        let mut exact_scored: Vec<VectorSearchHit> = dest
            .iter()
            .filter_map(|neighbor| {
                let index = neighbor.index;
                if let Some(allowed) = allowed {
                    if !allowed.contains(index as u32) {
                        return None;
                    }
                }
                let doc_id = self
                    .id_to_doc
                    .get(index)
                    .filter(|s| !s.is_empty())
                    .cloned()?;
                let stored = self.vectors.get(&doc_id)?;
                let score = super::vector::cosine_similarity(&query.vector, stored).unwrap_or(0.0);
                Some(VectorSearchHit {
                    document_id: doc_id,
                    score,
                })
            })
            .collect();
        exact_scored.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.document_id.cmp(&right.document_id))
        });
        if exact_scored.len() > limit {
            exact_scored.truncate(limit);
        }
        Ok(exact_scored)
    }
}

impl std::fmt::Debug for HnswBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HnswBackend")
            .field("metadata", &self.metadata)
            .field("document_count", &self.doc_to_id.len())
            .field("hnsw_len", &self.hnsw.len())
            .field("hnsw_layers", &self.hnsw.layers())
            .finish()
    }
}

impl HnswBackend {
    /// Persist the HNSW graph and its exact-vector cache to `path`.
    ///
    /// The contents are serialized with bincode and atomically swapped into
    /// place so readers never see a partial file. Callers should pair this
    /// with the sidecar integrity metadata so stale graphs can be rejected on
    /// load.
    pub fn persist_to_path(&self, path: &Path) -> io::Result<()> {
        let bytes = bincode::serde::encode_to_vec(self, bincode::config::standard())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = path.with_extension("tmp");
        let file = File::create(&temp_path)?;
        let mut writer = BufWriter::new(file);
        std::io::Write::write_all(&mut writer, &bytes)?;
        writer.flush()?;
        drop(writer);

        // fs::rename is atomic on POSIX and most modern filesystems.
        fs::rename(temp_path, path)
    }

    /// Load a persisted HNSW graph from `path`.
    ///
    /// Returns an error if the file is corrupt or if the persisted metadata
    /// does not match the supplied `metadata`. This prevents a graph built
    /// for one embedding space from being reused for another.
    pub fn load_from_path(path: &Path, metadata: &EmbeddingMetadata) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let backend: Self =
            bincode::serde::decode_from_reader(&mut reader, bincode::config::standard())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
        if &backend.metadata != metadata {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HNSW graph metadata does not match the requested embedding space",
            ));
        }
        Ok(backend)
    }
}
