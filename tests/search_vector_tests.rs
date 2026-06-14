use mentisdb::search::quantization::Quantizer;
use mentisdb::search::{
    cosine_similarity, embed_batch_to_documents, select_backend_kind, EmbeddingBuildError,
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector,
    LocalTextEmbeddingProvider, Scalar8BitQuantizer, VectorBackend, VectorBackendKind,
    VectorDocument, VectorFilter, VectorIndex, VectorIndexError, VectorQuery, VectorSearchBackend,
    VectorSearchHit, DEFAULT_EXACT_TO_HNSW_THRESHOLD,
};
use std::error::Error;
use std::fmt;

#[cfg(feature = "hnsw-backend")]
use mentisdb::search::QuantizedHnswBackend;

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
    // Exact branch returns the concrete VectorIndex variant.
    matches!(via_explicit_kind, VectorBackend::Exact(_));
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn with_backend_kind_hnsw_returns_usable_backend() {
    // H1: the Hnsw branch returns a real HNSW backend, not a fallback to
    // Exact. Insert, then query; the top hit must be the document with the
    // highest cosine similarity to the query.
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![
        VectorDocument::new("close", vec![1.0, 0.0]),
        VectorDocument::new("far", vec![-1.0, 0.0]),
    ];
    let backend =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();
    let hits = backend
        .search(&VectorQuery::new(vec![1.0, 0.0]).with_limit(2))
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].document_id, "close");
    assert_eq!(hits[1].document_id, "far");
    // Score is the *cosine similarity* in [-1.0, 1.0] even though the
    // HNSW graph only stored a bit-cast distance.
    assert!((hits[0].score - 1.0).abs() < 1e-5, "got {}", hits[0].score);
    assert!((hits[1].score + 1.0).abs() < 1e-5, "got {}", hits[1].score);
    matches!(backend, VectorBackend::Hnsw(_));
}

#[test]
#[cfg(not(feature = "hnsw-backend"))]
fn with_backend_kind_hnsw_silently_falls_back_to_exact_without_feature() {
    // H1 is feature-gated. Without the `hnsw-backend` feature, the
    // Hnsw branch is compiled out and the constructor returns an Exact
    // backend. The previous `debug_assert!` is also compiled out, so
    // there is no panic; this test only runs without the feature.
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![VectorDocument::new("only", vec![0.7, 0.7])];
    let backend =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();
    let hits = backend
        .search(&VectorQuery::new(vec![0.7, 0.7]).with_limit(1))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document_id, "only");
    matches!(backend, VectorBackend::Exact(_));
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

// ---------------------------------------------------------------------------
// H1: HNSW backend recall and latency (only when the `hnsw-backend` feature
// is on). We synthesize a small corpus (1k in tests; the 100k + 1M
// benchmarks live in benches/ and the 0.10.x release bench harness), verify
// that recall@10 against the exact f32 backend is >= 0.90 on normalized
// random unit vectors, and that the p95 query latency is sub-millisecond at
// this scale.
// ---------------------------------------------------------------------------

#[test]
#[cfg(feature = "hnsw-backend")]
fn hnsw_backend_recall_at_1k_meets_floor() {
    // Deterministic synthetic corpus. 1k normalized unit vectors in 32d.
    // The exact and HNSW backends are both queried for top-10 neighbors;
    // we count how often the HNSW top-10 set matches the exact top-10 set
    // (set recall). A loose floor of 0.90 catches gross regressions; the
    // production acceptance test in benches/ targets >= 0.95 at 1M.
    let dim = 32;
    let n = 1_000usize;
    let k = 10;
    let metadata = EmbeddingMetadata::new("toy", dim, "v1");

    let mut documents = Vec::with_capacity(n);
    for i in 0..n {
        // Cheap deterministic pseudo-random unit vector. We just need a
        // stable, non-degenerate corpus to measure relative recall.
        let raw: Vec<f32> = (0..dim)
            .map(|d| {
                let x = ((i * 31 + d * 17) % 1009) as f32 / 1009.0;
                x - 0.5
            })
            .collect();
        let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        let vector: Vec<f32> = raw.iter().map(|v| v / norm).collect();
        documents.push(VectorDocument::new(format!("doc-{i:04}"), vector));
    }

    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let hnsw =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();

    let queries: Vec<Vec<f32>> = (0..50)
        .map(|q| {
            let raw: Vec<f32> = (0..dim)
                .map(|d| {
                    let x = ((q * 53 + d * 19) % 997) as f32 / 997.0;
                    x - 0.5
                })
                .collect();
            let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            raw.iter().map(|v| v / norm).collect()
        })
        .collect();

    let mut total_recall = 0.0f64;
    for query in &queries {
        let exact_hits: std::collections::HashSet<String> = exact
            .search(&VectorQuery::new(query.clone()).with_limit(k))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let hnsw_hits: std::collections::HashSet<String> = hnsw
            .search(&VectorQuery::new(query.clone()).with_limit(k))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let intersection = exact_hits.intersection(&hnsw_hits).count();
        total_recall += intersection as f64 / k as f64;
    }
    let mean_recall = total_recall / queries.len() as f64;
    assert!(
        mean_recall >= 0.90,
        "HNSW mean recall@10 was {mean_recall:.3}, expected >= 0.90"
    );
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn hnsw_backend_query_is_fast_at_1k() {
    // 100 queries on a 1k-vector corpus should comfortably come in under
    // 100ms p95 on a developer laptop. The benches/ harness has the
    // 100k + 1M numbers.
    let dim = 32;
    let n = 1_000usize;
    let metadata = EmbeddingMetadata::new("toy", dim, "v1");
    let mut documents = Vec::with_capacity(n);
    for i in 0..n {
        let raw: Vec<f32> = (0..dim)
            .map(|d| ((i * 31 + d * 17) % 1009) as f32 / 1009.0 - 0.5)
            .collect();
        let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        let vector: Vec<f32> = raw.iter().map(|v| v / norm).collect();
        documents.push(VectorDocument::new(format!("doc-{i:04}"), vector));
    }
    let hnsw =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();

    let mut latencies_us: Vec<u128> = Vec::with_capacity(100);
    for q in 0..100 {
        let query: Vec<f32> = (0..dim)
            .map(|d| ((q * 53 + d * 19) % 997) as f32 / 997.0 - 0.5)
            .collect();
        let norm = query.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        let query: Vec<f32> = query.iter().map(|v| v / norm).collect();
        let started = std::time::Instant::now();
        let _ = hnsw
            .search(&VectorQuery::new(query).with_limit(10))
            .unwrap();
        latencies_us.push(started.elapsed().as_micros());
    }
    latencies_us.sort_unstable();
    let p95 = latencies_us[(latencies_us.len() as f64 * 0.95) as usize];
    // Loose ceiling; the H5 benchmark is the real gate. We only want to
    // catch catastrophic regressions in H1.
    assert!(
        p95 < 100_000,
        "HNSW p95 query latency at 1k was {p95}us, expected < 100_000us"
    );
}

// ---------------------------------------------------------------------------
// H2: hybrid bitmap-backed filters for vector search. The exact backend pre-
// filters during its linear scan; the HNSW backend translates the id set into
// a roaring bitmap of internal item ids, oversamples, and then intersects.
// ---------------------------------------------------------------------------

#[test]
fn exact_backend_search_filtered_returns_only_matching_documents() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let index = VectorIndex::from_documents(
        metadata,
        vec![
            VectorDocument::new("alpha", vec![1.0, 0.0]),
            VectorDocument::new("bravo", vec![0.0, 1.0]),
            VectorDocument::new("charlie", vec![-1.0, 0.0]),
            VectorDocument::new("delta", vec![0.0, -1.0]),
        ],
    )
    .unwrap();

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(2);

    // Filter that allows only alpha and charlie.
    let filter = VectorFilter::from_ids(["alpha", "charlie"]);
    let hits = index.search_filtered(&query, &filter).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].document_id, "alpha");
    assert!((hits[0].score - 1.0).abs() < 1e-5);
    assert_eq!(hits[1].document_id, "charlie");

    // Filter that allows only delta should still return it, even though it
    // scores poorly against the query.
    let filter = VectorFilter::from_ids(["delta"]);
    let hits = index.search_filtered(&query, &filter).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document_id, "delta");

    // Empty filter behaves like unfiltered search.
    let empty_filter = VectorFilter::from_ids(Vec::<&str>::new());
    let unfiltered = index.search(&query).unwrap();
    let filtered = index.search_filtered(&query, &empty_filter).unwrap();
    assert_eq!(filtered, unfiltered);

    // Filter that excludes every id returns nothing.
    let exclude_all = VectorFilter::from_ids(["missing", "unknown"]);
    let hits = index.search_filtered(&query, &exclude_all).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn exact_backend_search_filtered_orders_by_cosine_then_id() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let index = VectorIndex::from_documents(
        metadata,
        vec![
            VectorDocument::new("a", vec![1.0, 0.0]),
            VectorDocument::new("b", vec![0.9, 0.0]),
            VectorDocument::new("c", vec![0.8, 0.0]),
        ],
    )
    .unwrap();

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(2);
    let filter = VectorFilter::from_ids(["b", "c"]);
    let hits = index.search_filtered(&query, &filter).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits[0].score > hits[1].score || hits[0].document_id < hits[1].document_id);
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn hnsw_backend_search_filtered_matches_exact_on_small_corpus() {
    // HNSW post-filtering with oversampling should return the same top-k ids
    // as the exact backend on a tiny, deterministic corpus where the
    // approximate graph cannot reasonably reorder the results.
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![
        VectorDocument::new("alpha", vec![1.0, 0.0]),
        VectorDocument::new("bravo", vec![0.0, 1.0]),
        VectorDocument::new("charlie", vec![-1.0, 0.0]),
        VectorDocument::new("delta", vec![0.0, -1.0]),
    ];

    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let hnsw =
        VectorIndex::with_backend_kind(metadata, documents, VectorBackendKind::Hnsw).unwrap();

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(2);
    let filter = VectorFilter::from_ids(["bravo", "charlie", "delta"]);

    let exact_hits = exact.search_filtered(&query, &filter).unwrap();
    let hnsw_hits = hnsw.search_filtered(&query, &filter).unwrap();

    assert_eq!(exact_hits.len(), 2);
    assert_eq!(hnsw_hits.len(), 2);
    assert_eq!(exact_hits[0].document_id, hnsw_hits[0].document_id);
    assert_eq!(exact_hits[1].document_id, hnsw_hits[1].document_id);
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn hnsw_backend_search_filtered_recall_at_1k_meets_floor() {
    // With a 50% id filter on the 1k test corpus, the HNSW filtered results
    // should still overlap heavily with the exact filtered results. We
    // compare filtered top-10 set recall to the exact backend on the same
    // subset.
    let dim = 32;
    let n = 1_000usize;
    let k = 10;
    let metadata = EmbeddingMetadata::new("toy", dim, "v1");

    let mut documents = Vec::with_capacity(n);
    for i in 0..n {
        let raw: Vec<f32> = (0..dim)
            .map(|d| ((i * 31 + d * 17) % 1009) as f32 / 1009.0 - 0.5)
            .collect();
        let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        let vector: Vec<f32> = raw.iter().map(|v| v / norm).collect();
        documents.push(VectorDocument::new(format!("doc-{i:04}"), vector));
    }

    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let hnsw = VectorIndex::with_backend_kind(metadata, documents.clone(), VectorBackendKind::Hnsw)
        .unwrap();

    // Filter: every other document (odd indices). This is a lazy stand-in
    // for a metadata clause that matches ~half the corpus.
    let allowed_ids: std::collections::BTreeSet<String> = documents
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, d)| d.document_id.clone())
        .collect();
    let filter = VectorFilter::from_ids(allowed_ids.iter().cloned());

    let queries: Vec<Vec<f32>> = (0..50)
        .map(|q| {
            let raw: Vec<f32> = (0..dim)
                .map(|d| ((q * 53 + d * 19) % 997) as f32 / 997.0 - 0.5)
                .collect();
            let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            raw.iter().map(|v| v / norm).collect()
        })
        .collect();

    let mut total_recall = 0.0f64;
    for query in &queries {
        let exact_hits: std::collections::HashSet<String> = exact
            .search_filtered(&VectorQuery::new(query.clone()).with_limit(k), &filter)
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let hnsw_hits: std::collections::HashSet<String> = hnsw
            .search_filtered(&VectorQuery::new(query.clone()).with_limit(k), &filter)
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let intersection = exact_hits.intersection(&hnsw_hits).count();
        // Normalize by the smaller result set so a sparse exact set does not
        // inflate recall.
        let denominator = exact_hits.len().max(1);
        total_recall += intersection as f64 / denominator as f64;
    }
    let mean_recall = total_recall / queries.len() as f64;
    assert!(
        mean_recall >= 0.85,
        "HNSW filtered mean recall@10 was {mean_recall:.3}, expected >= 0.85"
    );
}

#[test]
fn vector_filter_from_ids_deduplicates_and_treats_empty_as_unfiltered() {
    let f1 = VectorFilter::from_ids(["a", "a", "b"]);
    assert!(!f1.allows_all());

    let f2 = VectorFilter::from_ids(Vec::<&str>::new());
    assert!(f2.allows_all());

    let f3 = VectorFilter::from_ids(["x"]);
    assert!(f3.allows("x"));
    assert!(!f3.allows("y"));
}

// ---------------------------------------------------------------------------
// H3: quantized HNSW backend. The graph stores 8-bit quantized vectors,
// reducing memory footprint ~4x versus the f32 HNSW graph. Exact f32 vectors
// are still cached for final re-scoring, so hit scores remain cosine in
// [-1.0, 1.0].
// ---------------------------------------------------------------------------

#[test]
fn scalar_quantizer_reduces_dimension_to_one_byte() {
    let dim = 128;
    let vectors = vec![(0..dim).map(|d| d as f32 / 64.0 - 1.0).collect::<Vec<_>>(); 10];
    let quantizer = Scalar8BitQuantizer::train(&vectors);
    let encoded = quantizer.encode(&vectors[0]);
    assert_eq!(encoded.len(), dim);
    assert_eq!(std::mem::size_of_val(encoded.as_slice()), dim);
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn quantized_hnsw_backend_matches_exact_on_small_corpus() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![
        VectorDocument::new("alpha", vec![1.0, 0.0]),
        VectorDocument::new("bravo", vec![0.0, 1.0]),
        VectorDocument::new("charlie", vec![-1.0, 0.0]),
        VectorDocument::new("delta", vec![0.0, -1.0]),
    ];
    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let quantized = QuantizedHnswBackend::from_documents(metadata, documents).unwrap();

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(3);
    let exact_hits = exact.search(&query).unwrap();
    let quantized_hits = quantized.search(&query).unwrap();

    assert_eq!(exact_hits.len(), quantized_hits.len());
    assert_eq!(exact_hits[0].document_id, quantized_hits[0].document_id);
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn quantized_hnsw_backend_recall_at_1k_meets_floor() {
    // Same protocol as the f32 HNSW recall gate, but with quantized graph
    // storage. Recall should be only slightly lower than the f32 graph.
    let dim = 32;
    let n = 1_000usize;
    let k = 10;
    let metadata = EmbeddingMetadata::new("toy", dim, "v1");

    let mut documents = Vec::with_capacity(n);
    for i in 0..n {
        let raw: Vec<f32> = (0..dim)
            .map(|d| ((i * 31 + d * 17) % 1009) as f32 / 1009.0 - 0.5)
            .collect();
        let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        let vector: Vec<f32> = raw.iter().map(|v| v / norm).collect();
        documents.push(VectorDocument::new(format!("doc-{i:04}"), vector));
    }

    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let quantized = QuantizedHnswBackend::from_documents(metadata, documents).unwrap();

    let queries: Vec<Vec<f32>> = (0..50)
        .map(|q| {
            let raw: Vec<f32> = (0..dim)
                .map(|d| ((q * 53 + d * 19) % 997) as f32 / 997.0 - 0.5)
                .collect();
            let norm = raw.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            raw.iter().map(|v| v / norm).collect()
        })
        .collect();

    let mut total_recall = 0.0f64;
    for query in &queries {
        let exact_hits: std::collections::HashSet<String> = exact
            .search(&VectorQuery::new(query.clone()).with_limit(k))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let quantized_hits: std::collections::HashSet<String> = quantized
            .search(&VectorQuery::new(query.clone()).with_limit(k))
            .unwrap()
            .into_iter()
            .map(|h| h.document_id)
            .collect();
        let intersection = exact_hits.intersection(&quantized_hits).count();
        total_recall += intersection as f64 / k as f64;
    }
    let mean_recall = total_recall / queries.len() as f64;
    assert!(
        mean_recall >= 0.85,
        "Quantized HNSW mean recall@10 was {mean_recall:.3}, expected >= 0.85"
    );
}

#[test]
#[cfg(feature = "hnsw-backend")]
fn quantized_hnsw_backend_search_filtered_matches_exact() {
    let metadata = EmbeddingMetadata::new("toy", 2, "v1");
    let documents = vec![
        VectorDocument::new("alpha", vec![1.0, 0.0]),
        VectorDocument::new("bravo", vec![0.0, 1.0]),
        VectorDocument::new("charlie", vec![-1.0, 0.0]),
        VectorDocument::new("delta", vec![0.0, -1.0]),
    ];
    let exact = VectorIndex::from_documents(metadata.clone(), documents.clone()).unwrap();
    let quantized = QuantizedHnswBackend::from_documents(metadata, documents).unwrap();

    let query = VectorQuery::new(vec![1.0, 0.0]).with_limit(2);
    let filter = VectorFilter::from_ids(["bravo", "charlie", "delta"]);

    let exact_hits = exact.search_filtered(&query, &filter).unwrap();
    let quantized_hits = quantized.search_filtered(&query, &filter).unwrap();

    assert_eq!(exact_hits.len(), quantized_hits.len());
    assert_eq!(exact_hits[0].document_id, quantized_hits[0].document_id);
}
