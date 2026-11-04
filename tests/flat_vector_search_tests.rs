//! The exact index scores with a lane-parallel dot product over a flat,
//! aligned vector store. These tests pin that it ranks exactly as the scorer it
//! replaced (a left-to-right `cosine_similarity` per stored vector, ties broken
//! by document id) on randomized corpora, that scores stay within the documented
//! epsilon, and that an index built from rows shares them instead of copying.

use mentisdb::search::flat_store::{VectorRow, VectorStore, DOT_EPSILON};
use mentisdb::search::{
    cosine_similarity, EmbeddingMetadata, VectorDocument, VectorFilter, VectorIndex, VectorQuery,
    VectorSearchBackend,
};

struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.unit().abs() * 2.0 * 1e6) as usize % bound
    }
}

/// The scorer the exact index used before the flat store: sequential cosine,
/// zero for a zero vector, best first, document id breaks ties.
fn reference(
    corpus: &[(String, Vec<f32>)],
    query: &[f32],
    limit: usize,
    allowed: impl Fn(&str) -> bool,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = corpus
        .iter()
        .filter(|(id, _)| allowed(id))
        .map(|(id, vector)| (id.clone(), cosine_similarity(query, vector).unwrap_or(0.0)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(limit.max(1));
    scored
}

fn corpus(
    rng: &mut Lcg,
    size: usize,
    dimension: usize,
    clusters: usize,
) -> Vec<(String, Vec<f32>)> {
    let centres: Vec<Vec<f32>> = (0..clusters)
        .map(|_| (0..dimension).map(|_| rng.unit()).collect())
        .collect();
    (0..size)
        .map(|i| {
            let centre = &centres[rng.below(clusters)];
            let mut vector: Vec<f32> = centre.iter().map(|c| c + rng.unit() * 0.5).collect();
            // Some exact duplicates (equal scores, id ties) and some zero vectors.
            if i % 17 == 5 {
                vector = centre.clone();
            }
            if i % 97 == 3 {
                vector = vec![0.0; dimension];
            }
            (format!("doc-{i:05}"), vector)
        })
        .collect()
}

/// The flat index returns the same ids in the same order as the reference
/// scorer, with scores within `DOT_EPSILON`, across dimensions that are and are
/// not multiples of the 16-lane line, with duplicates, zero vectors and a limit
/// larger than the corpus.
#[test]
fn test_flat_scan_ranks_exactly_like_the_sequential_scorer() {
    let mut rng = Lcg(2024);
    for dimension in [1, 7, 16, 33, 128, 384] {
        let documents = corpus(&mut rng, 600, dimension, 12);
        let metadata = EmbeddingMetadata::new("diff", dimension, "v1");
        let index = VectorIndex::from_documents(
            metadata,
            documents
                .iter()
                .map(|(id, v)| VectorDocument::new(id.clone(), v.clone()))
                .collect(),
        )
        .unwrap();
        for _ in 0..40 {
            let query: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
            for limit in [1, 10, 50, 10_000] {
                let hits = index
                    .search(&VectorQuery::new(query.clone()).with_limit(limit))
                    .unwrap();
                let expected = reference(&documents, &query, limit, |_| true);
                assert_eq!(hits.len(), expected.len());
                let exact: std::collections::HashMap<&str, f32> = documents
                    .iter()
                    .map(|(id, v)| (id.as_str(), cosine_similarity(&query, v).unwrap_or(0.0)))
                    .collect();
                for (hit, (id, score)) in hits.iter().zip(&expected) {
                    assert!(
                        (hit.score - exact[hit.document_id.as_str()]).abs() <= DOT_EPSILON,
                        "{dimension}d: score of {} drifted",
                        hit.document_id
                    );
                    // Same document at every rank, except where the sequential
                    // scorer itself cannot tell the two apart (within epsilon).
                    if &hit.document_id != id {
                        assert!(
                            (exact[hit.document_id.as_str()] - score).abs() <= 2.0 * DOT_EPSILON,
                            "{dimension}d limit {limit}: {} ranked where {id} was",
                            hit.document_id
                        );
                    }
                }
            }
        }
    }
}

/// Filtering, replacement and removal keep agreeing with the reference.
#[test]
fn test_filter_upsert_and_remove_agree_with_the_reference() {
    let mut rng = Lcg(77);
    let dimension = 48;
    let mut documents = corpus(&mut rng, 300, dimension, 6);
    let metadata = EmbeddingMetadata::new("diff", dimension, "v1");
    let mut index = VectorIndex::from_documents(
        metadata,
        documents
            .iter()
            .map(|(id, v)| VectorDocument::new(id.clone(), v.clone()))
            .collect(),
    )
    .unwrap();
    // Replace a few, remove a few.
    for target in [3usize, 120, 250] {
        let replacement: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
        index
            .upsert_document(VectorDocument::new(
                documents[target].0.clone(),
                replacement.clone(),
            ))
            .unwrap();
        documents[target].1 = replacement;
    }
    for target in [10usize, 11, 200] {
        assert!(index.remove_document(&documents[target].0));
    }
    let removed: Vec<String> = [10usize, 11, 200]
        .iter()
        .map(|&i| documents[i].0.clone())
        .collect();
    documents.retain(|(id, _)| !removed.contains(id));
    assert_eq!(index.document_count(), documents.len());
    let allowed_ids: Vec<String> = documents
        .iter()
        .step_by(3)
        .map(|(id, _)| id.clone())
        .collect();
    let filter = VectorFilter::from_ids(allowed_ids.clone());
    for _ in 0..30 {
        let query: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
        let hits = index
            .search_filtered(&VectorQuery::new(query.clone()).with_limit(15), &filter)
            .unwrap();
        let expected = reference(&documents, &query, 15, |id| {
            allowed_ids.iter().any(|a| a == id)
        });
        assert_eq!(
            hits.iter()
                .map(|h| h.document_id.clone())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        );
        let all = index
            .search(&VectorQuery::new(query.clone()).with_limit(15))
            .unwrap();
        assert_eq!(
            all.iter()
                .map(|h| h.document_id.clone())
                .collect::<Vec<_>>(),
            reference(&documents, &query, 15, |_| true)
                .iter()
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// An index built from rows keeps the caller's rows: the same chunk and row at
/// the same address, not a copy. `get_vector` still returns the values.
#[test]
fn test_index_built_from_rows_shares_the_callers_storage() {
    let dimension = 20;
    let mut store = VectorStore::new(dimension);
    let mut rng = Lcg(5);
    let rows: Vec<(String, VectorRow)> = (0..300)
        .map(|i| {
            let values: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
            (format!("doc-{i}"), store.push(&values))
        })
        .collect();
    let index = VectorIndex::from_rows(
        EmbeddingMetadata::new("share", dimension, "v1"),
        rows.clone(),
    )
    .unwrap();
    for (id, row) in &rows {
        let held = index.shared_row(id).expect("indexed");
        assert!(held.same_row(row), "{id} was copied");
        assert_eq!(held.lines().as_ptr(), row.lines().as_ptr());
        assert_eq!(index.get_vector(id).unwrap(), row.to_vec());
    }
    // The trait's default path (used by backends that cannot share) copies.
    let mut by_value = VectorIndex::new(EmbeddingMetadata::new("share", dimension, "v1"));
    VectorSearchBackend::upsert_document(
        &mut by_value,
        VectorDocument::new("x", rows[0].1.to_vec()),
    )
    .unwrap();
    assert!(!by_value.shared_row("x").unwrap().same_row(&rows[0].1));
}

/// A vector that does not fit the space is rejected, shared or not.
#[test]
fn test_rows_of_the_wrong_dimension_or_non_finite_are_rejected() {
    let mut index = VectorIndex::new(EmbeddingMetadata::new("bad", 3, "v1"));
    assert!(index
        .upsert_row("short".into(), VectorRow::standalone(&[1.0, 2.0]))
        .is_err());
    assert!(index
        .upsert_row("nan".into(), VectorRow::standalone(&[1.0, f32::NAN, 2.0]))
        .is_err());
    assert_eq!(index.document_count(), 0);
}

/// Regression: remove-then-add churn used to push a new slot per re-added id
/// (and by-value replacement left the old row in the append-only store), so the
/// index grew without bound. Slots are reused and the store is compacted, and
/// ranking stays correct through the churn.
#[test]
fn test_remove_and_readd_churn_does_not_grow_the_index() {
    let dimension = 16;
    let mut rng = Lcg(9);
    let mut index = VectorIndex::new(EmbeddingMetadata::new("churn", dimension, "v1"));
    let mut live: Vec<(String, Vec<f32>)> = Vec::new();
    for i in 0..50 {
        let v: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
        index
            .upsert_document(VectorDocument::new(format!("doc-{i:03}"), v.clone()))
            .unwrap();
        live.push((format!("doc-{i:03}"), v));
    }
    let (slots_before, rows_before) = index.capacity_for_tests();
    for round in 0..3000 {
        let at = round % live.len();
        let id = live[at].0.clone();
        assert!(index.remove_document(&id));
        let v: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
        index
            .upsert_document(VectorDocument::new(id.clone(), v.clone()))
            .unwrap();
        live[at].1 = v;
        // Replacing by value without removing also leaves old rows behind.
        let r = (round * 7 + 3) % live.len();
        let v: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
        index
            .upsert_document(VectorDocument::new(live[r].0.clone(), v.clone()))
            .unwrap();
        live[r].1 = v;
    }
    let (slots_after, rows_after) = index.capacity_for_tests();
    assert_eq!(slots_after, slots_before, "slots are reused");
    assert!(
        rows_after <= rows_before + 2 * 256 + live.len(),
        "store rows grew without bound: {rows_before} -> {rows_after}"
    );
    assert_eq!(index.document_count(), live.len());
    let query: Vec<f32> = (0..dimension).map(|_| rng.unit()).collect();
    let hits = index
        .search(&VectorQuery::new(query.clone()).with_limit(10))
        .unwrap();
    let expected = reference(&live, &query, 10, |_| true);
    assert_eq!(
        hits.iter()
            .map(|h| h.document_id.clone())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>()
    );
}

/// Regression: `dot` zips its operands, so rows of different dimension used to
/// score silently on the shorter prefix. Debug builds now assert.
#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "rows of different dimension")]
fn test_dot_rejects_rows_of_different_dimension() {
    let short = VectorRow::standalone(&[1.0; 16]);
    let long = VectorRow::standalone(&[1.0; 40]);
    mentisdb::search::flat_store::dot(short.lines(), long.lines());
}
