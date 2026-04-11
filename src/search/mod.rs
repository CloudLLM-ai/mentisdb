//! Search-specific derived state and ranking helpers.
//!
//! These modules build rebuildable indexes over committed thoughts without
//! changing the append-only chain itself.

/// Seed-anchored context bundle rendering over graph-expansion hits.
pub mod bundle;
/// Deterministic breadth-first expansion helpers built on top of the adjacency
/// layer.
pub mod expansion;
/// Graph adjacency and edge-provenance structures derived from committed
/// thoughts.
pub mod graph;
/// Irregular verb lemma expansion for lexical search queries.
pub mod lemmas;
/// BM25-style lexical indexing and ranking over committed thoughts.
pub mod lexical;
/// Provenance path structures for graph expansion starting from lexical seeds.
pub mod provenance;
/// Reciprocal Rank Fusion (RRF) reranking for hybrid search results.
pub mod ranked;
/// Rebuildable vector sidecar persistence for one durable chain.
pub mod sidecar;
/// Provider-agnostic vector and embedding helpers for deterministic ranking.
pub mod vector;

/// Real semantic embedding provider backed by the `fastembed` crate.
#[cfg(feature = "local-embeddings")]
pub mod fastembed_provider;

pub use bundle::{
    build_context_bundles, ContextBundle, ContextBundleHit, ContextBundleOptions,
    ContextBundleResult, ContextBundleSeed,
};
pub use expansion::{
    GraphExpansionHit, GraphExpansionMode, GraphExpansionQuery, GraphExpansionResult,
    GraphExpansionStats,
};
pub use graph::{
    AdjacencyDirection, GraphEdge, GraphEdgeProvenance, ThoughtAdjacencyIndex, ThoughtLocator,
};
pub use provenance::{GraphExpansionHop, GraphExpansionPath, GraphExpansionPathError};
pub use sidecar::{
    VectorSidecar, VectorSidecarEntry, VectorSidecarFreshness, VectorSidecarIntegrity,
    VECTOR_SIDECAR_SCHEMA_VERSION,
};
pub use vector::{
    cosine_similarity, embed_batch_to_documents, EmbeddingBuildError, EmbeddingInput,
    EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, LocalTextEmbeddingError,
    LocalTextEmbeddingProvider, VectorDocument, VectorIndex, VectorIndexError, VectorQuery,
    VectorSearchHit, LOCAL_TEXT_EMBEDDING_DIMENSION, LOCAL_TEXT_EMBEDDING_MODEL_ID,
    LOCAL_TEXT_EMBEDDING_VERSION,
};

#[cfg(feature = "local-embeddings")]
pub use fastembed_provider::{
    FastEmbedError, FastEmbedProvider, FASTEMBED_MINILM_DIMENSION, FASTEMBED_MINILM_MODEL_ID,
    FASTEMBED_MINILM_VERSION,
};
