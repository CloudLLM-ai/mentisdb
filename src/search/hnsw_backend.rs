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
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use bincode;
use hnsw::{Hnsw, Params};
use rand_pcg::Pcg64;
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("MENTISDB_HNSW_EF_SEARCH")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|&value| value > 0)
            .unwrap_or(HNSW_EF_SEARCH)
    })
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
    /// `allowed` is an optional bitmap of HNSW item ids.
    ///
    /// * Unfiltered: ask the graph for exactly `limit` neighbours.
    /// * Filtered, selective filter (see [`use_exact_filtered_scan`]): the
    ///   approximate graph is bypassed and exact cosine is computed over the
    ///   allowed ids only. A post-filter over an oversampled graph result
    ///   loses recall for selective filters because almost none of the
    ///   nearest neighbours of the query are allowed.
    /// * Filtered, permissive filter: oversample by
    ///   [`filtered_candidate_count`], keep the allowed neighbours, and
    ///   re-score them with exact cosine.
    ///
    /// Only the neighbours actually returned by the graph are considered; the
    /// destination buffer's unused tail never leaks into the results.
    fn search_inner(
        &self,
        query: &VectorQuery,
        allowed: Option<&RoaringBitmap>,
    ) -> Result<Vec<VectorSearchHit>, VectorIndexError> {
        super::vector::validate_vector(&query.vector, self.metadata.dimension, None, "query")?;

        if self.doc_to_id.is_empty() {
            return Ok(Vec::new());
        }

        let corpus = self.doc_to_id.len();
        let limit = query.limit.max(1).min(corpus);
        if let Some(allowed) = allowed {
            if use_exact_filtered_scan(allowed.len() as usize, corpus) {
                return Ok(self.exact_search_over(&query.vector, allowed.iter(), limit));
            }
        }
        let candidate_count = match allowed {
            Some(allowed) => filtered_candidate_count(limit, corpus, allowed.len() as usize),
            None => limit,
        };
        let ef = hnsw_ef_search().max(candidate_count);
        let mut dest = vec![
            Neighbor {
                distance: 0,
                index: 0,
            };
            candidate_count
        ];
        let mut searcher = hnsw::Searcher::default();
        let found = self
            .hnsw
            .nearest(&query.vector, ef, &mut searcher, &mut dest);

        let ids = found.iter().filter_map(|neighbor| {
            let index = neighbor.index;
            match allowed {
                Some(allowed) if !allowed.contains(index as u32) => None,
                _ => Some(index as u32),
            }
        });
        Ok(self.exact_search_over(&query.vector, ids, limit))
    }

    /// Score the given HNSW item ids with exact cosine similarity and return
    /// the best `limit` hits, best first (ties broken by document id).
    ///
    /// Ids that do not resolve to a live document are skipped.
    fn exact_search_over(
        &self,
        query: &[f32],
        ids: impl Iterator<Item = u32>,
        limit: usize,
    ) -> Vec<VectorSearchHit> {
        let mut scored: Vec<(f32, &str)> = ids
            .filter_map(|index| {
                let doc_id = self
                    .id_to_doc
                    .get(index as usize)
                    .filter(|s| !s.is_empty())?;
                let stored = self.vectors.get(doc_id)?;
                let score = super::vector::cosine_similarity(query, stored).unwrap_or(0.0);
                Some((score, doc_id.as_str()))
            })
            .collect();
        let order = |left: &(f32, &str), right: &(f32, &str)| {
            right.0.total_cmp(&left.0).then_with(|| left.1.cmp(right.1))
        };
        if scored.len() > limit {
            scored.select_nth_unstable_by(limit - 1, order);
            scored.truncate(limit);
        }
        scored.sort_by(order);
        scored
            .into_iter()
            .map(|(score, doc_id)| VectorSearchHit {
                document_id: doc_id.to_string(),
                score,
            })
            .collect()
    }
}

/// Largest allowed-set size for which filtered search bypasses the HNSW graph
/// and scores every allowed document exactly.
pub const EXACT_FILTER_MAX_ALLOWED: usize = 4096;

/// Return `true` when a filter that allows `allowed` of `corpus` documents is
/// selective enough to be answered by an exact scan of the allowed ids.
///
/// That is the case when at most [`EXACT_FILTER_MAX_ALLOWED`] documents are
/// allowed, or when fewer than 5% of the corpus is allowed. An exact scan of
/// a small allowed set is both faster than an oversampled graph walk and
/// exact (no recall loss).
///
/// # Examples
///
/// ```
/// use mentisdb::search::hnsw_backend::use_exact_filtered_scan;
///
/// assert!(use_exact_filtered_scan(30, 1_000_000));
/// assert!(use_exact_filtered_scan(40_000, 1_000_000));
/// assert!(!use_exact_filtered_scan(500_000, 1_000_000));
/// ```
pub fn use_exact_filtered_scan(allowed: usize, corpus: usize) -> bool {
    allowed <= EXACT_FILTER_MAX_ALLOWED || allowed.saturating_mul(20) < corpus
}

/// Number of graph candidates to request for a permissive filtered search.
///
/// Scales the oversampling with `corpus / allowed` (at least 4x, so that
/// roughly `limit` allowed neighbours are expected among the candidates),
/// with a floor of 64 and a cap of the corpus size.
///
/// # Examples
///
/// ```
/// use mentisdb::search::hnsw_backend::filtered_candidate_count;
///
/// assert_eq!(filtered_candidate_count(10, 100_000, 50_000), 64);
/// assert_eq!(filtered_candidate_count(100, 100_000, 10_000), 1_000);
/// assert_eq!(filtered_candidate_count(10, 50, 40), 50);
/// ```
pub fn filtered_candidate_count(limit: usize, corpus: usize, allowed: usize) -> usize {
    let ratio = corpus.div_ceil(allowed.max(1)).max(4);
    limit.saturating_mul(ratio).max(64).min(corpus)
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

/// Magic bytes opening every persisted HNSW graph file (format version 2).
///
/// Version 1 files (bincode body only, no header) are recognised by the
/// missing magic and rebuilt: the graph is derived state, so refusing a legacy
/// file costs one rebuild and nothing else.
pub const GRAPH_MAGIC: &[u8; 8] = b"MDBHNSW2";

/// Length in bytes of the graph file header: [`GRAPH_MAGIC`] followed by the
/// raw 32-byte content digest.
pub const GRAPH_HEADER_LEN: usize = 8 + 32;

/// Order-independent SHA-256 digest of an embedding space and its documents.
///
/// The digest binds the model id, embedding version and dimension together
/// with the set of `(document id, vector)` pairs, so a graph built for another
/// model, or from a corpus that merely has the same number of entries, never
/// matches. Each document is hashed on its own and the results are summed
/// lane-wise, which makes the value independent of insertion order (the
/// sidecar lists documents in chain order, the backend stores them sorted).
///
/// # Examples
///
/// ```
/// use mentisdb::search::hnsw_backend::documents_digest;
/// use mentisdb::search::{EmbeddingMetadata, VectorDocument};
///
/// let meta = EmbeddingMetadata::new("toy", 2, "v1");
/// let a = VectorDocument::new("a", vec![1.0, 0.0]);
/// let b = VectorDocument::new("b", vec![0.0, 1.0]);
/// let forward = documents_digest(&meta, [&a, &b].into_iter().map(|d| (d.document_id.as_str(), d.vector.as_slice())));
/// let reverse = documents_digest(&meta, [&b, &a].into_iter().map(|d| (d.document_id.as_str(), d.vector.as_slice())));
/// assert_eq!(forward, reverse);
/// let other = VectorDocument::new("b", vec![0.0, -1.0]);
/// let changed = documents_digest(&meta, [&a, &other].into_iter().map(|d| (d.document_id.as_str(), d.vector.as_slice())));
/// assert_ne!(forward, changed);
/// ```
pub fn documents_digest<'a>(
    metadata: &EmbeddingMetadata,
    documents: impl IntoIterator<Item = (&'a str, &'a [f32])>,
) -> [u8; 32] {
    let mut lanes = [0_u64; 4];
    let mut count = 0_u64;
    for (id, vector) in documents {
        let mut hasher = Sha256::new();
        hasher.update((id.len() as u64).to_le_bytes());
        hasher.update(id.as_bytes());
        for value in vector {
            hasher.update(value.to_le_bytes());
        }
        let one: [u8; 32] = hasher.finalize().into();
        for (lane, chunk) in lanes.iter_mut().zip(one.chunks_exact(8)) {
            let word = u64::from_le_bytes(chunk.try_into().expect("8-byte chunk"));
            *lane = lane.wrapping_add(word);
        }
        count += 1;
    }
    let mut hasher = Sha256::new();
    for field in [
        metadata.model_id.as_str(),
        metadata.embedding_version.as_str(),
    ] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.update((metadata.dimension as u64).to_le_bytes());
    hasher.update(count.to_le_bytes());
    for lane in lanes {
        hasher.update(lane.to_le_bytes());
    }
    hasher.finalize().into()
}

impl HnswBackend {
    /// Digest of the live documents currently held by this backend; see
    /// [`documents_digest`].
    pub fn content_digest(&self) -> [u8; 32] {
        documents_digest(
            &self.metadata,
            self.vectors
                .iter()
                .map(|(id, vector)| (id.as_str(), vector.as_slice())),
        )
    }

    /// Persist the HNSW graph and its exact-vector cache to `path`.
    ///
    /// The file is [`GRAPH_MAGIC`], the [`content_digest`](Self::content_digest)
    /// of the documents the graph was built from, then the bincode body. It is
    /// written to a unique sibling temp file and renamed into place, so readers
    /// never see a partial file. No fsync: the graph is derived, and a torn or
    /// stale file fails the digest check on load and is rebuilt.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from creating, writing or renaming the file.
    pub fn persist_to_path(&self, path: &Path) -> io::Result<()> {
        static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let body = bincode::serde::encode_to_vec(self, bincode::config::standard())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp_name = path.as_os_str().to_owned();
        temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let temp_path = std::path::PathBuf::from(temp_name);
        let result = (|| {
            let mut writer = BufWriter::new(File::create(&temp_path)?);
            writer.write_all(GRAPH_MAGIC)?;
            writer.write_all(&self.content_digest())?;
            writer.write_all(&body)?;
            writer.flush()?;
            drop(writer);
            fs::rename(&temp_path, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    /// Load a persisted HNSW graph from `path`.
    ///
    /// Returns an error if the file is corrupt, is a legacy (headerless)
    /// graph, if the body does not match the digest in its header, or if the
    /// persisted metadata does not match `metadata`. This prevents a graph
    /// built for one embedding space from being reused for another.
    ///
    /// # Errors
    ///
    /// `InvalidData` for any of the rejections above, or the underlying I/O
    /// error.
    pub fn load_from_path(path: &Path, metadata: &EmbeddingMetadata) -> io::Result<Self> {
        Self::load_verified(path, metadata, None)
    }

    /// Like [`load_from_path`](Self::load_from_path), and additionally require
    /// the graph to have been built from exactly the documents whose
    /// [`documents_digest`] is `expected`. A graph with the same entry count
    /// but different content is rejected.
    ///
    /// # Errors
    ///
    /// `InvalidData` when the digest differs, plus every error of
    /// [`load_from_path`](Self::load_from_path).
    pub fn load_from_path_matching(
        path: &Path,
        metadata: &EmbeddingMetadata,
        expected: &[u8; 32],
    ) -> io::Result<Self> {
        Self::load_verified(path, metadata, Some(expected))
    }

    fn load_verified(
        path: &Path,
        metadata: &EmbeddingMetadata,
        expected: Option<&[u8; 32]>,
    ) -> io::Result<Self> {
        let invalid =
            |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_owned());
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut header = [0_u8; GRAPH_HEADER_LEN];
        reader
            .read_exact(&mut header)
            .map_err(|_| invalid("HNSW graph file is shorter than its header"))?;
        if &header[..8] != GRAPH_MAGIC {
            return Err(invalid(
                "legacy or unrecognised HNSW graph file; rebuilding",
            ));
        }
        let stored: [u8; 32] = header[8..].try_into().expect("32-byte digest");
        if expected.is_some_and(|expected| expected != &stored) {
            return Err(invalid(
                "HNSW graph was built from different documents than the sidecar",
            ));
        }
        let backend: Self =
            bincode::serde::decode_from_reader(&mut reader, crate::decode_limits::file_config())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error}")))?;
        if &backend.metadata != metadata {
            return Err(invalid(
                "HNSW graph metadata does not match the requested embedding space",
            ));
        }
        if backend.content_digest() != stored {
            return Err(invalid("HNSW graph body does not match its header digest"));
        }
        Ok(backend)
    }
}
