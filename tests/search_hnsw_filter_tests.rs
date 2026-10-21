//! Regression tests for filtered HNSW search recall and result hygiene.

use std::collections::BTreeSet;

use mentisdb::search::hnsw_backend::{
    filtered_candidate_count, use_exact_filtered_scan, HnswBackend, EXACT_FILTER_MAX_ALLOWED,
};
use mentisdb::search::{
    cosine_similarity, EmbeddingMetadata, VectorDocument, VectorFilter, VectorQuery,
    VectorSearchBackend,
};

const DIM: usize = 16;

fn metadata() -> EmbeddingMetadata {
    EmbeddingMetadata::new("test-model", DIM, "v1")
}

/// Deterministic pseudo-random vector for document `i`.
fn vector_for(i: usize) -> Vec<f32> {
    let mut state = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..DIM)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ((state % 2000) as f32 / 1000.0) - 1.0
        })
        .collect()
}

fn build(count: usize) -> HnswBackend {
    let documents: Vec<VectorDocument> = (0..count)
        .map(|i| VectorDocument::new(format!("doc-{i:05}"), vector_for(i)))
        .collect();
    HnswBackend::from_documents(metadata(), documents).expect("hnsw build")
}

/// A selective filter must return the exact best allowed documents, even
/// though almost none of the query's global nearest neighbours are allowed.
#[test]
fn selective_filter_returns_exact_top_hits_over_allowed_ids() {
    let backend = build(1500);
    let query_vector = vector_for(7);
    // 25 allowed documents, none of them the query's own document.
    let allowed_ids: Vec<String> = (900..925).map(|i| format!("doc-{i:05}")).collect();
    let filter = VectorFilter::from_ids(allowed_ids.clone());
    let hits = backend
        .search_filtered(
            &VectorQuery::new(query_vector.clone()).with_limit(10),
            &filter,
        )
        .unwrap();

    let mut expected: Vec<(f32, String)> = allowed_ids
        .iter()
        .map(|id| {
            let index: usize = id.trim_start_matches("doc-").parse().unwrap();
            (
                cosine_similarity(&query_vector, &vector_for(index)).unwrap(),
                id.clone(),
            )
        })
        .collect();
    expected.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let expected_ids: Vec<&str> = expected
        .iter()
        .take(10)
        .map(|(_, id)| id.as_str())
        .collect();
    let got_ids: Vec<&str> = hits.iter().map(|hit| hit.document_id.as_str()).collect();
    assert_eq!(got_ids, expected_ids);
}

/// Results never contain duplicates or documents outside the filter (the old
/// code iterated an index-0-prefilled buffer and could emit spurious doc 0).
#[test]
fn filtered_results_contain_only_allowed_unique_documents() {
    let backend = build(300);
    let allowed: BTreeSet<String> = (100..140).map(|i| format!("doc-{i:05}")).collect();
    let filter = VectorFilter::from_ids(allowed.clone());
    let hits = backend
        .search_filtered(&VectorQuery::new(vector_for(5)).with_limit(100), &filter)
        .unwrap();
    assert_eq!(hits.len(), allowed.len());
    let unique: BTreeSet<&str> = hits.iter().map(|hit| hit.document_id.as_str()).collect();
    assert_eq!(unique.len(), hits.len());
    assert!(hits.iter().all(|hit| allowed.contains(&hit.document_id)));
    assert!(!hits.iter().any(|hit| hit.document_id == "doc-00000"));
}

/// Unfiltered search returns each document at most once, best first.
#[test]
fn unfiltered_search_is_sorted_and_unique() {
    let backend = build(300);
    let hits = backend
        .search(&VectorQuery::new(vector_for(11)).with_limit(20))
        .unwrap();
    assert_eq!(hits.len(), 20);
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    let unique: BTreeSet<&str> = hits.iter().map(|hit| hit.document_id.as_str()).collect();
    assert_eq!(unique.len(), hits.len());
    assert_eq!(hits[0].document_id, "doc-00011");
}

/// The exact-scan switch and oversampling policy.
#[test]
fn filter_policy_thresholds() {
    assert!(use_exact_filtered_scan(1, 10));
    assert!(use_exact_filtered_scan(
        EXACT_FILTER_MAX_ALLOWED,
        10_000_000
    ));
    assert!(use_exact_filtered_scan(40_000, 1_000_000));
    assert!(!use_exact_filtered_scan(60_000, 1_000_000));
    assert!(!use_exact_filtered_scan(
        EXACT_FILTER_MAX_ALLOWED + 1,
        50_000
    ));

    assert_eq!(filtered_candidate_count(10, 100_000, 50_000), 64);
    assert_eq!(filtered_candidate_count(100, 100_000, 10_000), 1_000);
    assert_eq!(filtered_candidate_count(10, 50, 40), 50);
    assert_eq!(filtered_candidate_count(5, 1_000_000, 0), 1_000_000);
}
