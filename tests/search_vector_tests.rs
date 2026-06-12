use mentisdb::search::{
    cosine_similarity, embed_batch_to_documents, select_backend_kind, EmbeddingBuildError,
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector,
    LocalTextEmbeddingProvider, VectorBackendKind, VectorDocument, VectorIndex, VectorIndexError,
    VectorQuery, VectorSearchBackend, VectorSearchHit, DEFAULT_EXACT_TO_HNSW_THRESHOLD,
};
use std::error::Error;
use std::fmt;

#[test]
fn cosine_similarity_returns_expected_values() {
    let identical = cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).unwrap();
    assert!((identical - 1.0).abs() < 1e-6);

    let orthogonal = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).unwrap();
    assert!(orthogonal.abs() < 1e-6);

    let opposite = cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]).unwrap();
    assert!((opposite + 1.0).abs() < 1e-6);
}

#[test]
fn cosine_similarity_rejects_mismatched_or_zero_vectors() {
    assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), None);
    assert_eq!(cosine_similarity(&[], &[]), None);
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), None);
}

#[test]
fn vector_index_ranks_deterministically_by_cosine_then_id() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let index = VectorIndex::from_documents(
        metadata,
        vec![
            VectorDocument::new("delta", vec![0.5, 0.5]),
            VectorDocument::new("alpha", vec![1.0, 0.0]),
            VectorDocument::new("bravo", vec![0.5, 0.5]),
            VectorDocument::new("charlie", vec![0.0, 1.0]),
        ],
    )
    .unwrap();

    let hits = index
        .search(&VectorQuery::new(vec![1.0, 0.0]).with_limit(10))
        .unwrap();

    assert_eq!(hits[0].document_id, "alpha");
    // Same score tie should sort by document id ascending.
    assert_eq!(hits[1].document_id, "bravo");
    assert_eq!(hits[2].document_id, "delta");
    assert_eq!(hits[3].document_id, "charlie");
    assert!(hits[0].score > hits[1].score);
    assert_eq!(hits[1].score, hits[2].score);
}

#[test]
fn vector_index_limit_and_upsert_behavior() {
    let metadata = EmbeddingMetadata::new("toy", 3, "v1");
    let mut index = VectorIndex::new(metadata);
    index
        .upsert_document(VectorDocument::new("doc-a", vec![1.0, 0.0, 0.0]))
        .unwrap();
    index
        .upsert_document(VectorDocument::new("doc-b", vec![0.0, 1.0, 0.0]))
        .unwrap();
    index
        .upsert_document(VectorDocument::new("doc-c", vec![0.0, 0.0, 1.0]))
        .unwrap();

    let top_one = index
        .search(&VectorQuery::new(vec![1.0, 0.0, 0.0]).with_limit(1))
        .unwrap();
    assert_eq!(top_one.len(), 1);
    assert_eq!(top_one[0].document_id, "doc-a");

    // Upsert should replace the existing vector for doc-b.
    index
        .upsert_document(VectorDocument::new("doc-b", vec![1.0, 0.0, 0.0]))
        .unwrap();
    let top_two = index
        .search(&VectorQuery::new(vec![1.0, 0.0, 0.0]).with_limit(2))
        .unwrap();
    assert_eq!(top_two[0].document_id, "doc-a");
    assert_eq!(top_two[1].document_id, "doc-b");
}

#[test]
fn vector_index_rejects_dimension_mismatch_and_non_finite_values() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let mut index = VectorIndex::new(metadata);

    let mismatch = index
        .upsert_document(VectorDocument::new("doc", vec![1.0, 2.0, 3.0]))
        .unwrap_err();
    assert_eq!(
        mismatch,
        VectorIndexError::DimensionMismatch {
            expected: 2,
            actual: 3,
            context: "document",
            document_id: Some("doc".to_string()),
        }
    );

    let non_finite = index
        .upsert_document(VectorDocument::new("doc", vec![1.0, f32::NAN]))
        .unwrap_err();
    assert_eq!(
        non_finite,
        VectorIndexError::NonFiniteValue {
            context: "document",
            document_id: Some("doc".to_string()),
            value_index: 1,
        }
    );

    index
        .upsert_document(VectorDocument::new("doc-ok", vec![1.0, 0.0]))
        .unwrap();
    let query_error = index.search(&VectorQuery::new(vec![1.0])).unwrap_err();
    assert_eq!(
        query_error,
        VectorIndexError::DimensionMismatch {
            expected: 2,
            actual: 1,
            context: "query",
            document_id: None,
        }
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DummyProviderError(&'static str);

impl fmt::Display for DummyProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for DummyProviderError {}

struct DummyProvider {
    metadata: EmbeddingMetadata,
    response: Result<Vec<EmbeddingVector>, DummyProviderError>,
}

impl DummyProvider {
    fn ok(metadata: EmbeddingMetadata, vectors: Vec<Vec<f32>>) -> Self {
        Self {
            metadata,
            response: Ok(vectors.into_iter().map(EmbeddingVector::new).collect()),
        }
    }

    fn fail(metadata: EmbeddingMetadata, message: &'static str) -> Self {
        Self {
            metadata,
            response: Err(DummyProviderError(message)),
        }
    }
}

impl EmbeddingProvider for DummyProvider {
    type Error = DummyProviderError;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, _inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        self.response.clone()
    }
}

#[test]
fn embed_batch_to_documents_maps_provider_output_to_input_ids() {
    let provider = DummyProvider::ok(
        EmbeddingMetadata::new("toy", 2, "v1"),
        vec![vec![1.0, 0.0], vec![0.0, 1.0]],
    );
    let inputs = vec![
        EmbeddingInput::new("doc-1", "first"),
        EmbeddingInput::new("doc-2", "second"),
    ];

    let docs = embed_batch_to_documents(&provider, &inputs).unwrap();
    assert_eq!(docs[0].document_id, "doc-1");
    assert_eq!(docs[0].vector, vec![1.0, 0.0]);
    assert_eq!(docs[1].document_id, "doc-2");
    assert_eq!(docs[1].vector, vec![0.0, 1.0]);
}

#[test]
fn embed_batch_to_documents_rejects_provider_shape_errors() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let inputs = vec![EmbeddingInput::new("doc-1", "first")];

    let provider_fail = DummyProvider::fail(metadata.clone(), "network");
    match embed_batch_to_documents(&provider_fail, &inputs).unwrap_err() {
        EmbeddingBuildError::Provider(error) => assert_eq!(error.to_string(), "network"),
        other => panic!("expected provider error, got {other:?}"),
    }

    let provider_count_mismatch = DummyProvider::ok(metadata.clone(), vec![]);
    assert_eq!(
        embed_batch_to_documents(&provider_count_mismatch, &inputs).unwrap_err(),
        EmbeddingBuildError::OutputCountMismatch {
            expected: 1,
            actual: 0,
        }
    );

    let provider_dimension_mismatch =
        DummyProvider::ok(metadata.clone(), vec![vec![1.0, 0.0, 0.0]]);
    assert_eq!(
        embed_batch_to_documents(&provider_dimension_mismatch, &inputs).unwrap_err(),
        EmbeddingBuildError::DimensionMismatch {
            expected: 2,
            actual: 3,
            input_index: 0,
        }
    );

    let provider_non_finite = DummyProvider::ok(metadata, vec![vec![1.0, f32::INFINITY]]);
    assert_eq!(
        embed_batch_to_documents(&provider_non_finite, &inputs).unwrap_err(),
        EmbeddingBuildError::NonFiniteValue {
            input_index: 0,
            value_index: 1,
        }
    );
}

#[test]
fn local_text_embedding_provider_is_deterministic_and_topic_sensitive() {
    let provider = LocalTextEmbeddingProvider::new();
    let docs = embed_batch_to_documents(
        &provider,
        &[
            EmbeddingInput::new("a", "Latency budget for database performance"),
            EmbeddingInput::new("b", "Performance budget for database latency"),
            EmbeddingInput::new("c", "Invoice reconciliation for vendor payments"),
        ],
    )
    .unwrap();
    let docs_repeat = embed_batch_to_documents(
        &provider,
        &[EmbeddingInput::new(
            "a",
            "Latency budget for database performance",
        )],
    )
    .unwrap();

    assert_eq!(docs[0].vector, docs_repeat[0].vector);
    let similar = cosine_similarity(&docs[0].vector, &docs[1].vector).unwrap();
    let different = cosine_similarity(&docs[0].vector, &docs[2].vector).unwrap();
    assert!(
        similar > different,
        "expected topical overlap to score higher"
    );
}

// ---------------------------------------------------------------------------
// H0: VectorSearchBackend trait + VectorBackendKind + threshold
// ---------------------------------------------------------------------------

#[test]
fn vector_backend_kind_as_str_is_stable() {
    assert_eq!(VectorBackendKind::Exact.as_str(), "exact");
    assert_eq!(VectorBackendKind::Hnsw.as_str(), "hnsw");
}

#[test]
fn select_backend_kind_uses_default_threshold() {
    assert_eq!(DEFAULT_EXACT_TO_HNSW_THRESHOLD, 50_000);
    // Below threshold -> Exact.
    assert_eq!(select_backend_kind(0), VectorBackendKind::Exact);
    assert_eq!(
        select_backend_kind(DEFAULT_EXACT_TO_HNSW_THRESHOLD - 1),
        VectorBackendKind::Exact
    );
    // At or above threshold -> Hnsw (boundary is "exclusively Hnsw").
    assert_eq!(
        select_backend_kind(DEFAULT_EXACT_TO_HNSW_THRESHOLD),
        VectorBackendKind::Hnsw
    );
    assert_eq!(select_backend_kind(50_001), VectorBackendKind::Hnsw);
    assert_eq!(select_backend_kind(10_000_000), VectorBackendKind::Hnsw);
}

#[test]
fn with_backend_kind_exact_matches_from_documents() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![
        VectorDocument::new("a", vec![1.0, 0.0]),
        VectorDocument::new("b", vec![0.0, 1.0]),
    ];

    let via_explicit_kind = VectorIndex::with_backend_kind(
        metadata.clone(),
        documents.clone(),
        VectorBackendKind::Exact,
    )
    .unwrap();
    let via_default = VectorIndex::from_documents(metadata, documents).unwrap();

    assert_eq!(via_explicit_kind.document_count(), 2);
    assert_eq!(
        via_explicit_kind.document_count(),
        via_default.document_count()
    );

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(2);
    let explicit_hits = via_explicit_kind.search(&query).unwrap();
    let default_hits = via_default.search(&query).unwrap();
    assert_eq!(explicit_hits, default_hits);
}

#[test]
fn with_backend_kind_hnsw_currently_falls_back_to_exact_in_h0() {
    // H0 does not implement the Hnsw backend yet. In release builds the
    // constructor succeeds and returns a usable Exact index. In debug builds
    // the implementation emits a `debug_assert!` so any caller who tries to
    // use the not-yet-shipped Hnsw backend gets a loud test failure; that
    // case is covered by `with_backend_kind_hnsw_loudly_panics_in_debug_builds`
    // below. This test runs in release builds only.
    if cfg!(debug_assertions) {
        return;
    }
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![VectorDocument::new("only", vec![0.7, 0.7])];
    let index =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();
    let hits = index
        .search(&VectorQuery::new(vec![0.7, 0.7]).with_limit(1))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document_id, "only");
}

#[test]
fn with_backend_kind_hnsw_loudly_panics_in_debug_builds() {
    if !cfg!(debug_assertions) {
        // Release builds strip the debug_assert; nothing to assert.
        return;
    }
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![VectorDocument::new("only", vec![0.7, 0.7])];
    let result = std::panic::catch_unwind(|| {
        let _ = VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw);
    });
    assert!(
        result.is_err(),
        "VectorBackendKind::Hnsw should panic under debug_assertions in H0"
    );
}

#[test]
fn trait_object_dispatch_matches_inherent_api() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let mut concrete = VectorIndex::from_documents(
        metadata,
        vec![
            VectorDocument::new("alpha", vec![1.0, 0.0]),
            VectorDocument::new("bravo", vec![0.0, 1.0]),
        ],
    )
    .unwrap();
    let trait_object: Box<dyn VectorSearchBackend> = Box::new(concrete.clone());

    // Read paths through the trait object.
    assert_eq!(trait_object.document_count(), 2);
    assert_eq!(trait_object.document_count(), concrete.document_count());
    assert_eq!(trait_object.metadata(), concrete.metadata());
    assert_eq!(trait_object.get_vector("alpha"), Some(vec![1.0, 0.0]));
    assert_eq!(trait_object.get_vector("missing"), None);

    let trait_hits = trait_object
        .search(&VectorQuery::new(vec![1.0, 0.0]).with_limit(2))
        .unwrap();
    let concrete_hits = concrete
        .search(&VectorQuery::new(vec![1.0, 0.0]).with_limit(2))
        .unwrap();
    assert_eq!(trait_hits, concrete_hits);

    // Write paths through the trait object.
    let mut trait_object: Box<dyn VectorSearchBackend> = Box::new(concrete.clone());
    trait_object
        .upsert_document(VectorDocument::new("charlie", vec![0.5, 0.5]))
        .unwrap();
    assert!(trait_object.remove_document("alpha"));
    assert_eq!(trait_object.document_count(), 2);
    assert_eq!(trait_object.get_vector("alpha"), None);
    assert_eq!(trait_object.get_vector("charlie"), Some(vec![0.5, 0.5]));

    // Trait object must reject dimension mismatches identically to the
    // inherent API.
    let bad = trait_object
        .upsert_document(VectorDocument::new("bad", vec![1.0, 0.0, 0.0]))
        .unwrap_err();
    let bad_inherent = concrete
        .upsert_document(VectorDocument::new("bad", vec![1.0, 0.0, 0.0]))
        .unwrap_err();
    assert_eq!(bad, bad_inherent);
    assert_eq!(
        bad,
        VectorIndexError::DimensionMismatch {
            expected: 2,
            actual: 3,
            context: "document",
            document_id: Some("bad".to_string()),
        }
    );
}

#[test]
fn vector_search_hit_is_partial_eq_usable_in_tests() {
    // Pin the equality semantics of the public hit type. This guards against
    // accidental field changes that would break the trait-object tests
    // above.
    let left = VectorSearchHit {
        document_id: "alpha".to_string(),
        score: 0.5,
    };
    let right = VectorSearchHit {
        document_id: "alpha".to_string(),
        score: 0.5,
    };
    assert_eq!(left, right);
}
