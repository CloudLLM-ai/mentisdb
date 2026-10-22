//! Regression tests for the exact vector backend's top-k selection and cached norms.

use mentisdb::search::{
    cosine_similarity, EmbeddingMetadata, VectorDocument, VectorFilter, VectorIndex, VectorQuery,
};

const DIM: usize = 12;

fn vector_for(i: usize) -> Vec<f32> {
    let mut state = (i as u64 + 7).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..DIM)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state % 2000) as f32 / 1000.0) - 1.0
        })
        .collect()
}

fn build(count: usize) -> VectorIndex {
    let mut index = VectorIndex::new(EmbeddingMetadata::new("m", DIM, "v1"));
    for i in 0..count {
        index
            .upsert_document(VectorDocument::new(format!("doc-{i:04}"), vector_for(i)))
            .unwrap();
    }
    index
}

fn brute_force(query: &[f32], count: usize, limit: usize) -> Vec<(String, f32)> {
    let mut all: Vec<(String, f32)> = (0..count)
        .map(|i| {
            (
                format!("doc-{i:04}"),
                cosine_similarity(query, &vector_for(i)).unwrap_or(0.0),
            )
        })
        .collect();
    all.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    all.truncate(limit);
    all
}

/// Exact search matches a brute-force `cosine_similarity` ranking, including
/// scores, for several limits.
#[test]
fn exact_search_matches_brute_force() {
    let index = build(200);
    let query = vector_for(3);
    for limit in [1usize, 5, 50, 200, 500] {
        let hits = index
            .search(&VectorQuery::new(query.clone()).with_limit(limit))
            .unwrap();
        let expected = brute_force(&query, 200, limit);
        assert_eq!(hits.len(), expected.len(), "limit {limit}");
        for (hit, (id, score)) in hits.iter().zip(expected) {
            assert_eq!(hit.document_id, id);
            assert!(
                (hit.score - score).abs() < 1e-6,
                "{} vs {}",
                hit.score,
                score
            );
        }
    }
}

/// Re-upserting a document replaces its vector and its cached norm.
#[test]
fn upsert_updates_cached_norm() {
    let mut index = build(20);
    let query = vector_for(5);
    // Replace doc-0003 with a vector 100x longer: cosine must not change.
    let scaled: Vec<f32> = vector_for(3).iter().map(|v| v * 100.0).collect();
    let before = index
        .search(&VectorQuery::new(query.clone()).with_limit(20))
        .unwrap();
    let before_score = before
        .iter()
        .find(|h| h.document_id == "doc-0003")
        .unwrap()
        .score;
    index
        .upsert_document(VectorDocument::new("doc-0003", scaled))
        .unwrap();
    let after = index
        .search(&VectorQuery::new(query).with_limit(20))
        .unwrap();
    let after_score = after
        .iter()
        .find(|h| h.document_id == "doc-0003")
        .unwrap()
        .score;
    assert!((before_score - after_score).abs() < 1e-5);
}

/// Zero vectors have no direction and score 0 rather than NaN.
#[test]
fn zero_vector_documents_score_zero() {
    let mut index = build(5);
    index
        .upsert_document(VectorDocument::new("zero", vec![0.0; DIM]))
        .unwrap();
    let hits = index
        .search(&VectorQuery::new(vector_for(1)).with_limit(10))
        .unwrap();
    let zero = hits.iter().find(|h| h.document_id == "zero").unwrap();
    assert_eq!(zero.score, 0.0);
    assert!(hits.iter().all(|h| h.score.is_finite()));
}

/// Filtered exact search honours both the filter and the limit.
#[test]
fn filtered_exact_search_respects_filter_and_limit() {
    let index = build(100);
    let allowed: Vec<String> = (10..30).map(|i| format!("doc-{i:04}")).collect();
    let filter = VectorFilter::from_ids(allowed.clone());
    let hits = index
        .search_filtered(&VectorQuery::new(vector_for(1)).with_limit(5), &filter)
        .unwrap();
    assert_eq!(hits.len(), 5);
    assert!(hits.iter().all(|h| allowed.contains(&h.document_id)));
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
}
