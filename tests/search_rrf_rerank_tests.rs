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

/// With no vector sidecar and no graph, only the lexical list is populated.
/// The reranked head must be the top lexical hits (not the oldest documents
/// that a zero-score vector/graph list would boost) and no tail hit may
/// outrank them.
#[test]
fn rerank_without_sidecar_keeps_top_lexical_hits_on_top() {
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
    let head: std::collections::BTreeSet<u64> = reranked.hits[..20]
        .iter()
        .map(|hit| hit.thought.index)
        .collect();
    assert_eq!(head, expected, "head must be the top-20 lexical hits");
    assert!(reranked.hits[..20].iter().all(|hit| hit.score.rrf > 0.0));
    assert!(reranked.hits[20..].iter().all(|hit| hit.score.rrf == 0.0));
    let head_min_total = reranked.hits[..20]
        .iter()
        .map(|hit| hit.score.total)
        .fold(f32::INFINITY, f32::min);
    // The tail keeps raw BM25-scale totals, yet must still be ranked below the head.
    assert!(reranked.hits[20..]
        .iter()
        .any(|hit| hit.score.total > head_min_total));
    let positions: Vec<u64> = reranked.hits.iter().map(|hit| hit.thought.index).collect();
    for tail in &positions[20..] {
        assert!(!expected.contains(tail));
    }
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
