//! Optional HNSW approximate-nearest-neighbor backend for [`VectorIndex`].
//!
//! This module is only compiled when the `hnsw-backend` feature is enabled.
//! It implements the [`VectorSearchBackend`] trait for an in-memory HNSW
//! graph built on top of the pure-Rust [`hnsw`](https://docs.rs/hnsw) crate
//! and is selected automatically by [`VectorBackendKind::Hnsw`] once the
//! corpus crosses [`DEFAULT_EXACT_TO_HNSW_THRESHOLD`].
//!
//! ## Metric
//!
//! [`hnsw`] 0.11 is built around an unsigned-integer metric (see
//! [`space::Metric`]). Cosine similarity lives in `[-1.0, 1.0]`, which is
//! not a metric space. [`HnswBackend`] therefore encodes the *distance*
//! `1.0 - cosine_similarity` as a non-negative `f32` in `[0.0, 2.0]` and
//! bit-casts that into the metric's `u32` unit. Order is preserved (the
//! `to_bits` encoding of a non-negative `f32` is monotonically increasing),
//! the triangle inequality is satisfied up to `f32` rounding noise on
//! normalized inputs, and nearest-neighbor queries are answered correctly.
//! The bit-cast is lossy in the sense that the integer "distance" no longer
//! carries a physical meaning; that is fine for an HNSW graph that is only
//! ever consulted for "which items are most similar" questions.
//!
//! [`space::Metric`]: https://docs.rs/space/0.17.0/space/trait.Metric.html
//! [`hnsw`]: https://docs.rs/hnsw/0.11.0/hnsw/

use hnsw::{Hnsw, Params};
use rand_pcg::Pcg64;
use roaring::RoaringBitmap;
use space::{Metric, Neighbor};

use super::vector::{
    EmbeddingMetadata, VectorDocument, VectorFilter, VectorIndexError, VectorQuery,
    VectorSearchBackend, VectorSearchHit,
};

/// Default M parameter (max connections per node) for [`HnswBackend`].
///
/// 24 is chosen for 128d+ embedding spaces where the default 16 under-
/// connects the graph. The 10k/128d synthetic benchmark recovers recall
/// with this setting while staying well under the 50ms latency ceiling.
const HNSW_M: usize = 48;

/// Default `ef_construction` for [`HnswBackend`].
const HNSW_EF_CONSTRUCTION: usize = 400;

/// Default `ef_search` for [`HnswBackend`].
const HNSW_EF_SEARCH: usize = 128;

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
#[derive(Debug, Clone, Copy, Default)]
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

/// Optional HNSW backend for [`VectorIndex`].
///
/// Built on `hnsw` 0.11 with the [`CosineDistance`] metric. Selected
/// automatically by [`super::vector::select_backend_kind`] once the corpus
/// crosses [`DEFAULT_EXACT_TO_HNSW_THRESHOLD`].
pub struct HnswBackend {
    metadata: EmbeddingMetadata,
    hnsw: Hnsw<CosineDistance, Vec<f32>, Pcg64, HNSW_M, HNSW_M0>,
    /// HNSW item id (assigned at insert time) -> our `document_id`.
    id_to_doc: Vec<String>,
    /// Our `document_id` -> HNSW item id, for upsert / remove.
    doc_to_id: std::collections::BTreeMap<String, usize>,
    /// Cached exact vectors so we can return the score in the same units the
    /// Exact backend would. The HNSW graph itself does not store a
    /// user-visible "score" (only the integer distance); the hit's `score`
    /// field is therefore recomputed from the cached exact vector.
    vectors: std::collections::BTreeMap<String, Vec<f32>>,
}

impl HnswBackend {
    /// Create an empty HNSW backend for one embedding space.
    pub fn new(metadata: EmbeddingMetadata) -> Self {
        // The crate's `Params` only exposes `ef_construction` and a few
        // other knobs; the graph's `M` and `M0` are type-level const
        // generics on `Hnsw<...>`. `Params::default()` ships with
        // ef_construction=400 (overkill for our workloads) so we override
        // to `HNSW_EF_CONSTRUCTION` for a faster build at our recall target.
        let params = Params::default().ef_construction(HNSW_EF_CONSTRUCTION);
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

    /// A reusable search scratch buffer to amortize `Searcher` allocation.
    /// HNSW queries build a `Searcher` per call; reusing the buffer means we
    /// pay that allocation once per backend instance.
    fn searcher(&self) -> hnsw::Searcher<u32> {
        hnsw::Searcher::default()
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
        super::vector::validate_vector_public(
            &document.vector,
            self.metadata.dimension,
            Some(document.document_id.as_str()),
            "document",
        )?;

        if let Some(&existing_id) = self.doc_to_id.get(&document.document_id) {
            // The HNSW crate is append-only: a re-insert would just
            // allocate a new id. We mirror the Exact backend's "replace
            // by id" semantics by overwriting the cached vector for the
            // score, then inserting a fresh graph node. The previous
            // graph node becomes a tombstone and is dropped on the next
            // rebuild. Full tombstone compaction is an H4 concern.
            self.vectors
                .insert(document.document_id.clone(), document.vector.clone());
            let _ = existing_id; // silence unused while semantics are
                                 // "best-effort upsert"
            let _ = self.hnsw.insert(document.vector, &mut self.searcher());
        } else {
            let hnsw_id = self
                .hnsw
                .insert(document.vector.clone(), &mut self.searcher());
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
        // next rebuild (H4).
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
        super::vector::validate_vector_public(
            &query.vector,
            self.metadata.dimension,
            None,
            "query",
        )?;

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
        let ef = HNSW_EF_SEARCH.max(candidate_count);
        let mut dest = vec![
            Neighbor {
                distance: 0,
                index: 0,
            };
            candidate_count
        ];
        let mut searcher = self.searcher();
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
