//! # Search & Retrieval
//!
//! This module contains everything related to querying and ranking thoughts.
//!
//! MentisDB's retrieval system is **hybrid by design** and **extensible**:
//!
//! - Lexical (BM25 with per-field DF gating + automatic thesaurus expansion since 0.9.9)
//! - Dense vector similarity (via pluggable embedding providers)
//! - Graph expansion (explicit relations + implicit cosine-based edges)
//! - Session cohesion, importance, recency, and RRF reranking
//!
//! ## For Custom Integration Developers
//!
//! When embedding MentisDB, you will primarily interact with:
//!
//! - [`crate::RankedSearchQuery`] — the main query builder (recommended for almost all use cases)
//! - [`query_ranked`](crate::MentisDb::query_ranked) on [`crate::MentisDb`]
//! - The various index types if you want to build custom retrieval pipelines
//!
//! The automatic thesaurus expansion (introduced in 0.9.9) is applied transparently
//! inside the server layer for daemon users, and can be used directly when embedding
//! by calling [`thesaurus::expand_text`] and passing the result
//! to [`RankedSearchQuery::with_synonyms`](crate::RankedSearchQuery::with_synonyms).
//!
//! Most harness authors should **not** need to touch the internal index types directly.
//! Use the high-level [`crate::RankedSearchQuery`] API unless you are doing advanced research
//! or replacing large parts of the retrieval stack.

/// Seed-anchored context bundle rendering over graph-expansion hits.
pub mod bundle;
/// Embedding-based nearest-neighbor synonym generator.
pub mod embedding_synonyms;
/// Deterministic breadth-first expansion helpers built on top of the adjacency
/// layer.
pub mod expansion;
/// Graph adjacency and edge-provenance structures derived from committed
/// thoughts.
pub mod graph;
/// Vector-similarity implicit edge overlay for augmenting graph expansion.
pub mod implicit_edges;
/// Irregular verb lemma expansion for lexical search queries.
pub mod lemmas;
/// BM25-style lexical indexing and ranking over committed thoughts.
pub mod lexical;
/// Personalized PageRank graph expansion primitives.
pub mod ppr;
/// Provenance path structures for graph expansion starting from lexical seeds.
pub mod provenance;
/// Pseudo-relevance feedback query expansion primitives.
pub mod query_expansion;
/// Deterministic query intent classification and route weighting.
pub mod query_intent;
/// Reciprocal Rank Fusion (RRF) reranking for hybrid search results.
pub mod ranked;
/// Rebuildable vector sidecar persistence for one durable chain.
pub mod sidecar;
/// Append-only hierarchical summary candidate selection.
pub mod summary_index;
/// Static thesaurus for query-time synonym expansion.
pub mod thesaurus;
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
pub use implicit_edges::{ImplicitEdgeOverlay, ImplicitNeighbor};
pub use provenance::{GraphExpansionHop, GraphExpansionPath, GraphExpansionPathError};
pub use sidecar::{
    VectorSidecar, VectorSidecarEntry, VectorSidecarFreshness, VectorSidecarIntegrity,
    VECTOR_SIDECAR_SCHEMA_VERSION,
};
pub use summary_index::{
    build_summary_candidates, SummaryBuildConfig, SummaryCandidate, SummaryCoverage, SummaryGroup,
    SummarySourceThought,
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
