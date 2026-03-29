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
/// BM25-style lexical indexing and ranking over committed thoughts.
pub mod lexical;
/// Provenance path structures for graph expansion starting from lexical seeds.
pub mod provenance;
/// Rebuildable vector sidecar persistence for one durable chain.
pub mod sidecar;
/// Provider-agnostic vector and embedding helpers for deterministic ranking.
pub mod vector;

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
