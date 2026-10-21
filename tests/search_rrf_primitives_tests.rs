//! Regression tests for the RRF primitives in `mentisdb::search::ranked`.

use mentisdb::search::ranked::{rrf_merge, rrf_union_scores, top_positive_ranking, RRF_K};

/// Perfectly symmetric lists produce exact score ties; the merge order must
/// be deterministic (ascending id) instead of depending on `HashMap` order.
#[test]
fn rrf_merge_breaks_ties_by_ascending_id() {
    let a: Vec<u64> = (0..50).collect();
    let b: Vec<u64> = (0..50).rev().collect();
    let first = rrf_merge(&[&a[..], &b[..]], RRF_K);
    for _ in 0..20 {
        assert_eq!(rrf_merge(&[&a[..], &b[..]], RRF_K), first);
    }
    // ids i and 49-i have identical scores; the lower id must come first.
    let ids: Vec<u64> = first.iter().map(|(id, _)| *id).collect();
    let pos = |id: u64| ids.iter().position(|x| *x == id).unwrap();
    for i in 0..25u64 {
        assert!(pos(i) < pos(49 - i));
    }
}

/// Zero-score candidates must never enter an RRF rank list.
#[test]
fn top_positive_ranking_excludes_zero_scores_and_orders_deterministically() {
    let scored = [(5u64, 0.0f32), (1, 2.0), (2, 2.0), (3, 9.0), (4, -1.0)];
    assert_eq!(top_positive_ranking(&scored, 10), vec![3, 1, 2]);
    assert_eq!(top_positive_ranking(&scored, 2), vec![3, 1]);
    assert!(top_positive_ranking(&scored, 0).is_empty());
    assert!(top_positive_ranking::<u64>(&[], 3).is_empty());
}

/// An all-zero signal contributes nothing: only the lexical top-k are in the union.
#[test]
fn rrf_union_scores_ignores_absent_signals() {
    let lexical: Vec<(u64, f32)> = (0..200u64).map(|i| (i, 200.0 - i as f32)).collect();
    let zeros: Vec<(u64, f32)> = (0..200u64).map(|i| (i, 0.0)).collect();
    let scores = rrf_union_scores(&lexical, &zeros, &zeros, 20);
    assert_eq!(scores.len(), 20);
    assert!((0..20u64).all(|i| scores.contains_key(&i)));
    let expected = 1.0 / (RRF_K as f64 + 1.0);
    assert!((scores[&0] - expected).abs() < 1e-12);
}
