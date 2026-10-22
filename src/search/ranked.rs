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

/// Multiplier applied to a fused RRF score before it is added to a hit's
/// default ranked total.
///
/// Raw RRF scores are tiny (a rank-1 hit in one list is `1/61`), while the
/// default total is BM25-scale (typically 3 to 30). Scaling by 50 turns a
/// rank-1 hit in all three lists into a bonus of about 2.5, which breaks ties
/// and rewards agreement between signals without overriding a clearly
/// stronger lexical match.
pub const RRF_SCORE_SCALE: f32 = 50.0;

/// Merge ranked lists via Reciprocal Rank Fusion.
///
/// Each input is a slice of document identifiers (e.g. thought indices or UUIDs)
/// sorted by decreasing relevance within their respective ranking
/// (lexical-only or vector-only). Documents that appear in several lists
/// receive a contribution from each; documents in only one list receive a
/// single contribution. Empty lists contribute nothing.
///
/// Returns the merged list sorted by decreasing RRF score. Ties are broken by
/// ascending identifier so the output is deterministic regardless of
/// `HashMap` iteration order.
///
/// # Examples
///
/// ```
/// use mentisdb::search::ranked::{rrf_merge, RRF_K};
///
/// let a = [2usize, 1];
/// let b = [1usize, 2];
/// // Perfectly symmetric: the tie is broken by ascending id.
/// let merged = rrf_merge(&[&a[..], &b[..]], RRF_K);
/// assert_eq!(merged[0].0, 1);
/// ```
pub fn rrf_merge<T: Clone + std::hash::Hash + Eq + Ord>(lists: &[&[T]], k: usize) -> Vec<(T, f64)> {
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
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Merge three ranked lists (lexical, vector, graph) via RRF.
///
/// Convenience wrapper around [`rrf_merge`] for the common case where
/// graph-expanded hits form a third ranking signal alongside lexical
/// and vector scores.
pub fn rrf_merge_three<T: Clone + std::hash::Hash + Eq + Ord>(
    lexical: &[T],
    vector: &[T],
    graph: &[T],
    k: usize,
) -> Vec<(T, f64)> {
    rrf_merge(&[lexical, vector, graph], k)
}

/// Return the identifiers of the `k` best candidates that have a strictly
/// positive score, best first.
///
/// Candidates with a non-positive (or NaN) score carry no ranking signal for
/// the corresponding retriever, so they must not enter an RRF rank list: with
/// no vector sidecar every vector score is `0.0` and including them would hand
/// the `k` lowest-index documents a spurious `1/(k+rank)` boost. Ties are
/// broken by ascending identifier for determinism.
///
/// # Examples
///
/// ```
/// use mentisdb::search::ranked::top_positive_ranking;
///
/// let scored = [(1u64, 0.0f32), (2, 3.0), (3, 5.0), (4, 3.0)];
/// assert_eq!(top_positive_ranking(&scored, 2), vec![3, 2]);
/// assert!(top_positive_ranking(&[(1u64, 0.0f32)], 5).is_empty());
/// ```
pub fn top_positive_ranking<K: Clone + Ord>(scored: &[(K, f32)], k: usize) -> Vec<K> {
    let mut positive: Vec<&(K, f32)> = scored.iter().filter(|(_, score)| *score > 0.0).collect();
    let order = |a: &&(K, f32), b: &&(K, f32)| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    if k == 0 {
        return Vec::new();
    }
    if positive.len() > k {
        positive.select_nth_unstable_by(k - 1, order);
        positive.truncate(k);
    }
    positive.sort_by(order);
    positive.into_iter().map(|(id, _)| id.clone()).collect()
}

/// Compute RRF scores for the union of the top-`k` positive candidates of
/// the lexical, vector, and graph signals.
///
/// Each signal is first reduced with [`top_positive_ranking`], so absent
/// signals (all-zero scores) contribute no rank list at all. Only identifiers
/// that appear in at least one list receive an entry; callers must treat
/// absent identifiers as having no RRF contribution *and* rank them after the
/// union, since their raw totals live on a different scale.
///
/// # Examples
///
/// ```
/// use mentisdb::search::ranked::rrf_union_scores;
///
/// let lexical = [(1u64, 9.0f32), (2, 5.0), (3, 1.0)];
/// let vector = [(1u64, 0.0f32), (2, 0.0), (3, 0.0)];
/// let scores = rrf_union_scores(&lexical, &vector, &[], 2);
/// assert_eq!(scores.len(), 2);
/// assert!(scores.contains_key(&1) && scores.contains_key(&2));
/// ```
pub fn rrf_union_scores<K: Clone + std::hash::Hash + Eq + Ord>(
    lexical: &[(K, f32)],
    vector: &[(K, f32)],
    graph: &[(K, f32)],
    k: usize,
) -> std::collections::HashMap<K, f64> {
    let lexical_ids = top_positive_ranking(lexical, k);
    let vector_ids = top_positive_ranking(vector, k);
    let graph_ids = top_positive_ranking(graph, k);
    rrf_merge_three(&lexical_ids, &vector_ids, &graph_ids, RRF_K)
        .into_iter()
        .collect()
}

/// Return the distance from `value` to the closest element of `sorted`, or
/// `u64::MAX` when `sorted` is empty.
///
/// `sorted` must be in ascending order. Uses a binary search, so session
/// cohesion over `H` hits and `S` seeds costs `O((H + S) log S)` instead of
/// `O(H * S)`.
///
/// # Examples
///
/// ```
/// use mentisdb::search::ranked::nearest_sorted_distance;
///
/// let seeds = [3u64, 10, 40];
/// assert_eq!(nearest_sorted_distance(&seeds, 10), 0);
/// assert_eq!(nearest_sorted_distance(&seeds, 7), 3);
/// assert_eq!(nearest_sorted_distance(&seeds, 100), 60);
/// assert_eq!(nearest_sorted_distance(&[], 5), u64::MAX);
/// ```
pub fn nearest_sorted_distance(sorted: &[u64], value: u64) -> u64 {
    let split = sorted.partition_point(|&item| item < value);
    let below = split
        .checked_sub(1)
        .map(|i| value - sorted[i])
        .unwrap_or(u64::MAX);
    let above = sorted
        .get(split)
        .map(|&item| item - value)
        .unwrap_or(u64::MAX);
    below.min(above)
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
