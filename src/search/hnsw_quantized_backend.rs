//! Optional quantized HNSW approximate-nearest-neighbor backend (H3).
//!
//! This module is an extension of [`super::hnsw_backend`] that stores vectors
//! in the HNSW graph as 8-bit quantized bytes instead of raw `f32`s. The
//! graph memory footprint drops by roughly 4x, while a side cache of exact
//! `f32` vectors is kept for final cosine re-scoring.
//!
//! The module is only compiled when the `hnsw-backend` feature is enabled.

use hnsw::{Hnsw, Params};
use rand_pcg::Pcg64;
use space::Neighbor;

use super::quantization::Quantizer;
use super::quantization::{QuantizedCosineDistance, QuantizedVector, Scalar8BitQuantizer};
use super::vector::{
    EmbeddingMetadata, VectorDocument, VectorFilter, VectorIndexError, VectorQuery,
    VectorSearchBackend, VectorSearchHit,
};

/// Default M parameter (max connections per node) for [`QuantizedHnswBackend`].
const HNSW_M: usize = 16;

/// Default `ef_construction` for [`QuantizedHnswBackend`].
const HNSW_EF_CONSTRUCTION: usize = 200;

/// Default `ef_search` for [`QuantizedHnswBackend`].
const HNSW_EF_SEARCH: usize = 64;

/// Top layer size (`M0`) for [`QuantizedHnswBackend`].
const HNSW_M0: usize = HNSW_M * 2;

/// HNSW backend that stores vectors in quantized form inside the graph.
///
/// Built on `hnsw` 0.11 with the [`QuantizedCosineDistance`] metric.
/// The graph stores [`QuantizedVector`] items; the query vector is
/// quantized with the same [`Scalar8BitQuantizer`] at search time. Final
/// hits are re-scored against the exact `f32` cache so the returned score
/// remains cosine similarity in `[-1.0, 1.0]`.
pub struct QuantizedHnswBackend {
    metadata: EmbeddingMetadata,
    hnsw: Hnsw<QuantizedCosineDistance, QuantizedVector, Pcg64, HNSW_M, HNSW_M0>,
    /// HNSW item id -> our `document_id`.
    id_to_doc: Vec<String>,
    /// Our `document_id` -> HNSW item id.
    doc_to_id: std::collections::BTreeMap<String, usize>,
    /// Cached exact vectors for final cosine re-scoring.
    vectors: std::collections::BTreeMap<String, Vec<f32>>,
    /// Shared quantizer for graph storage and query encoding.
    quantizer: Scalar8BitQuantizer,
}

impl QuantizedHnswBackend {
    /// Create an empty quantized HNSW backend for one embedding space.
    ///
    /// The initial quantizer uses a fallback range of `[-1.0, 1.0]`. It is
    /// replaced when the first document batch is loaded.
    pub fn new(metadata: EmbeddingMetadata) -> Self {
        let quantizer = Scalar8BitQuantizer::train(&[]);
        let params = Params::default().ef_construction(HNSW_EF_CONSTRUCTION);
        let hnsw =
            Hnsw::<QuantizedCosineDistance, QuantizedVector, Pcg64, HNSW_M, HNSW_M0>::new_params(
                QuantizedCosineDistance::new(quantizer),
                params,
            );
        Self {
            metadata,
            hnsw,
            id_to_doc: Vec::new(),
            doc_to_id: std::collections::BTreeMap::new(),
            vectors: std::collections::BTreeMap::new(),
            quantizer,
        }
    }

    /// Create a quantized HNSW backend preloaded with a corpus.
    pub fn from_documents(
        metadata: EmbeddingMetadata,
        documents: Vec<VectorDocument>,
    ) -> Result<Self, VectorIndexError> {
        let raw_vectors: Vec<Vec<f32>> = documents.iter().map(|d| d.vector.clone()).collect();
        let quantizer = Scalar8BitQuantizer::train(&raw_vectors);
        let mut backend = Self::with_quantizer(metadata, quantizer);
        for document in documents {
            backend.upsert_document(document)?;
        }
        Ok(backend)
    }

    /// Create an empty backend with a pre-trained quantizer.
    pub fn with_quantizer(metadata: EmbeddingMetadata, quantizer: Scalar8BitQuantizer) -> Self {
        let params = Params::default().ef_construction(HNSW_EF_CONSTRUCTION);
        let hnsw =
            Hnsw::<QuantizedCosineDistance, QuantizedVector, Pcg64, HNSW_M, HNSW_M0>::new_params(
                QuantizedCosineDistance::new(quantizer),
                params,
            );
        Self {
            metadata,
            hnsw,
            id_to_doc: Vec::new(),
            doc_to_id: std::collections::BTreeMap::new(),
            vectors: std::collections::BTreeMap::new(),
            quantizer,
        }
    }

    fn searcher(&self) -> hnsw::Searcher<u32> {
        hnsw::Searcher::default()
    }
}

impl VectorSearchBackend for QuantizedHnswBackend {
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

        let quantized = QuantizedVector::new(self.quantizer.encode(&document.vector));
        if let Some(&existing_id) = self.doc_to_id.get(&document.document_id) {
            self.vectors
                .insert(document.document_id.clone(), document.vector.clone());
            let _ = existing_id;
            let _ = self.hnsw.insert(quantized, &mut self.searcher());
        } else {
            let hnsw_id = self.hnsw.insert(quantized, &mut self.searcher());
            self.id_to_doc.push(document.document_id.clone());
            self.doc_to_id.insert(document.document_id.clone(), hnsw_id);
            self.vectors.insert(document.document_id, document.vector);
        }
        Ok(())
    }

    fn remove_document(&mut self, document_id: &str) -> bool {
        let removed = self.vectors.remove(document_id).is_some();
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

impl QuantizedHnswBackend {
    fn filter_to_bitmap(&self, filter: &VectorFilter) -> roaring::RoaringBitmap {
        match filter {
            VectorFilter::None => self
                .doc_to_id
                .values()
                .map(|&id| id as u32)
                .collect::<roaring::RoaringBitmap>(),
            VectorFilter::Ids(ids) => ids
                .iter()
                .filter_map(|id| self.doc_to_id.get(id).map(|&id| id as u32))
                .collect::<roaring::RoaringBitmap>(),
        }
    }

    fn search_inner(
        &self,
        query: &VectorQuery,
        allowed: Option<&roaring::RoaringBitmap>,
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
        let quantized_query = QuantizedVector::new(self.quantizer.encode(&query.vector));
        let _ = self
            .hnsw
            .nearest(&quantized_query, ef, &mut searcher, &mut dest);

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

impl std::fmt::Debug for QuantizedHnswBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuantizedHnswBackend")
            .field("metadata", &self.metadata)
            .field("document_count", &self.doc_to_id.len())
            .field("hnsw_len", &self.hnsw.len())
            .field("hnsw_layers", &self.hnsw.layers())
            .finish()
    }
}
