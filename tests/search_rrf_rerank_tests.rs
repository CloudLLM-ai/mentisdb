//! Regression tests for RRF reranking inside `MentisDb::query_ranked`.

use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::{MentisDb, RankedSearchQuery, ThoughtInput, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn open_chain(label: &str) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_rrf_rerank_{label}_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    MentisDb::open_with_key(&dir, label).unwrap()
}

/// 1000 thoughts, 200 of which contain "needle". Needle document `i` is padded
/// with `199 - i` filler words, so BM25 length normalization makes newer
/// documents strictly better lexical matches than older ones.
fn build_needle_chain(label: &str) -> MentisDb {
    let mut chain = open_chain(label);
    for i in 0..1000usize {
        let content = if i % 5 == 0 {
            let n = i / 5;
            format!("needle{}", " filler".repeat(199 - n))
        } else {
            format!("unrelated background note number {i}")
        };
        chain
            .append_thought("a", ThoughtInput::new(ThoughtType::Idea, content))
            .unwrap();
    }
    chain
}

/// With no vector sidecar and no graph, only the lexical list is populated:
/// exactly the top-20 lexical hits get an RRF score. Zero-score vector/graph
/// lists must not hand the oldest documents a spurious boost.
#[test]
fn rerank_without_sidecar_boosts_only_top_lexical_hits() {
    let chain = build_needle_chain("rrf-head");
    let plain = chain.query_ranked(&RankedSearchQuery::new().with_text("needle").with_limit(200));
    assert_eq!(plain.hits.len(), 200);
    let mut by_lexical: Vec<_> = plain.hits.iter().collect();
    by_lexical.sort_by(|a, b| b.score.lexical.total_cmp(&a.score.lexical));
    let expected: std::collections::BTreeSet<u64> = by_lexical
        .iter()
        .take(20)
        .map(|hit| hit.thought.index)
        .collect();

    let reranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("needle")
            .with_reranking(20)
            .with_limit(200),
    );
    assert_eq!(reranked.hits.len(), 200);
    let boosted: std::collections::BTreeSet<u64> = reranked
        .hits
        .iter()
        .filter(|hit| hit.score.rrf > 0.0)
        .map(|hit| hit.thought.index)
        .collect();
    assert_eq!(
        boosted, expected,
        "rrf must be set for the top-20 lexical hits only"
    );
    // The oldest needle documents are the worst lexical matches and must not
    // be boosted by an all-zero vector or graph list.
    assert!(reranked
        .hits
        .iter()
        .filter(|hit| hit.thought.index < 50)
        .all(|hit| hit.score.rrf == 0.0));
}

/// Reranking never drops the default path's best hit from the head, and the
/// RRF bonus is exactly `RRF_SCORE_SCALE * rrf` on top of the default total.
#[test]
fn rerank_keeps_default_top_hit_and_adds_scaled_bonus() {
    let chain = build_needle_chain("rrf-not-worse");
    let plain = chain.query_ranked(&RankedSearchQuery::new().with_text("needle").with_limit(200));
    let default_top = plain.hits[0].thought.index;
    let reranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("needle")
            .with_reranking(50)
            .with_limit(200),
    );
    let head: Vec<u64> = reranked.hits[..5]
        .iter()
        .map(|hit| hit.thought.index)
        .collect();
    assert!(
        head.contains(&default_top),
        "default top hit {default_top} missing from {head:?}"
    );
    assert_eq!(reranked.hits[0].thought.index, default_top);

    let default_totals: std::collections::HashMap<u64, f32> = plain
        .hits
        .iter()
        .map(|hit| (hit.thought.index, hit.score.total))
        .collect();
    for hit in &reranked.hits {
        let want = default_totals[&hit.thought.index]
            + mentisdb::search::ranked::RRF_SCORE_SCALE * hit.score.rrf;
        assert!(
            (hit.score.total - want).abs() < 1e-3,
            "index {}: {} vs {}",
            hit.thought.index,
            hit.score.total,
            want
        );
    }
}

/// On a lexical-only corpus, reranking returns the same top-k set as the
/// default path (it refines, never replaces, the default ordering).
#[test]
fn rerank_head_set_matches_default_head_set_lexical_only() {
    let chain = build_needle_chain("rrf-set");
    let query = RankedSearchQuery::new().with_text("needle");
    let default_head: std::collections::BTreeSet<u64> = chain
        .query_ranked(&query.clone().with_limit(10))
        .hits
        .iter()
        .map(|hit| hit.thought.index)
        .collect();
    let rerank_head: std::collections::BTreeSet<u64> = chain
        .query_ranked(&query.with_reranking(50).with_limit(10))
        .hits
        .iter()
        .map(|hit| hit.thought.index)
        .collect();
    assert_eq!(rerank_head, default_head);
}

/// Reranking must be deterministic across repeated calls.
#[test]
fn rerank_is_deterministic() {
    let chain = build_needle_chain("rrf-determinism");
    let query = RankedSearchQuery::new()
        .with_text("needle")
        .with_reranking(20)
        .with_limit(50);
    let first: Vec<u64> = chain
        .query_ranked(&query)
        .hits
        .iter()
        .map(|hit| hit.thought.index)
        .collect();
    for _ in 0..5 {
        let again: Vec<u64> = chain
            .query_ranked(&query)
            .hits
            .iter()
            .map(|hit| hit.thought.index)
            .collect();
        assert_eq!(again, first);
    }
}
