//! Reciprocal Rank Fusion (RRF) reranking for hybrid search results.
//!
//! RRF merges multiple ranked lists into a single ordering using the formula:
//!
//! ```text
//! score(d) = Σ 1 / (k + rank_i(d))
//! ```
//!
//! where `k` is a constant (default 60) that dampens the influence of
//! high ranks, and `rank_i(d)` is the rank of document `d` in the
//! i-th ranked list (1-indexed). Documents that appear in more lists
//! and at higher positions receive larger combined scores.

/// Default RRF damping constant. The standard value of 60 was proposed
/// in the original RRF paper and works well across diverse collections.
pub const RRF_K: usize = 60;

/// Merge two ranked lists via Reciprocal Rank Fusion.
///
/// Each input is a slice of document identifiers (e.g. thought indices or UUID
/// strings) sorted by decreasing relevance within their respective ranking
/// (lexical-only or vector-only). Documents that appear in both lists receive
/// contributions from both; documents in only one list receive a single
/// contribution.
///
/// Returns the merged list sorted by decreasing RRF score, along with each
/// document's raw RRF score.
pub fn rrf_merge<T: Clone + std::hash::Hash + Eq>(lists: &[&[T]], k: usize) -> Vec<(T, f64)> {
    let mut scores = std::collections::HashMap::new();

    for list in lists {
        for (rank, item) in list.iter().enumerate() {
            let rrf_rank = rank + 1;
            let contribution = 1.0 / (k as f64 + rrf_rank as f64);
            scores
                .entry(item.clone())
                .and_modify(|score| *score += contribution)
                .or_insert(contribution);
        }
    }

    let mut ranked: Vec<(T, f64)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

/// Merge three ranked lists (lexical, vector, graph) via RRF.
///
/// Convenience wrapper around [`rrf_merge`] for the common case where
/// graph-expanded hits form a third ranking signal alongside lexical
/// and vector scores.
pub fn rrf_merge_three<T: Clone + std::hash::Hash + Eq>(
    lexical: &[T],
    vector: &[T],
    graph: &[T],
    k: usize,
) -> Vec<(T, f64)> {
    rrf_merge(&[lexical, vector, graph], k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rrf_single_list() {
        let list = [10usize, 20, 30];
        let result = rrf_merge(&[&list[..]], RRF_K);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, 10);
        let expected_first = 1.0 / (60.0 + 1.0);
        let expected_last = 1.0 / (60.0 + 3.0);
        assert!((result[0].1 - expected_first).abs() < 1e-10);
        assert!((result[2].1 - expected_last).abs() < 1e-10);
    }

    #[test]
    fn test_rrf_two_lists_agreement() {
        let a = [1usize, 2, 3];
        let b = [1usize, 2, 3];
        let result = rrf_merge(&[&a[..], &b[..]], RRF_K);
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, 1);
        let expected = 2.0 * (1.0 / (60.0 + 1.0));
        assert!((result[0].1 - expected).abs() < 1e-10);
    }

    #[test]
    fn test_rrf_two_lists_disagreement() {
        let a = [10usize, 20, 30];
        let b = [30usize, 20, 10];
        let k = 1;
        let result = rrf_merge(&[&a[..], &b[..]], k);
        assert_eq!(result.len(), 3);
        let s10 = result.iter().find(|(id, _)| *id == 10).unwrap().1;
        let s20 = result.iter().find(|(id, _)| *id == 20).unwrap().1;
        let s30 = result.iter().find(|(id, _)| *id == 30).unwrap().1;
        assert!((s10 - s30).abs() < 1e-10);
        let expected_extreme = 1.0 / (1.0 + 1.0) + 1.0 / (1.0 + 3.0);
        let expected_middle = 1.0 / (1.0 + 2.0) + 1.0 / (1.0 + 2.0);
        assert!((s10 - expected_extreme).abs() < 1e-10);
        assert!((s20 - expected_middle).abs() < 1e-10);
        assert!(s10 > s20);
    }

    #[test]
    fn test_rrf_empty_lists() {
        let result: Vec<(usize, f64)> = rrf_merge(&[], RRF_K);
        assert!(result.is_empty());
    }

    #[test]
    fn test_rrf_one_empty_one_nonempty() {
        let a: Vec<usize> = vec![];
        let b = [1usize, 2];
        let result = rrf_merge(&[&a[..], &b[..]], RRF_K);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, 1);
    }

    #[test]
    fn test_rrf_three_lists() {
        let lex = vec![1usize, 2, 3];
        let vec = vec![3usize, 1, 4];
        let graph = vec![2usize, 4, 5];
        let result = rrf_merge_three(&lex, &vec, &graph, RRF_K);
        assert_eq!(result.len(), 5);
        let ids: Vec<usize> = result.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&5));
    }
}
