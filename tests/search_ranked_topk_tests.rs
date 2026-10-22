//! Regression tests for top-k selection in `MentisDb::query_ranked`.
//!
//! `query_ranked` scores every match, selects the best `limit` with a partial
//! selection, and materializes hit objects only for that head. The head must be
//! exactly the prefix of the full ordering.

use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::GraphExpansionMode;
use mentisdb::{
    MentisDb, RankedSearchGraph, RankedSearchQuery, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn build_chain() -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("mentisdb_topk_{}_{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain = MentisDb::open_with_key(&dir, "topk").unwrap();
    for i in 0..400usize {
        let content = match i % 16 {
            0 => format!("quasar telemetry report {}", "pad ".repeat(i % 17)),
            // Identical documents: exact score ties across indices.
            1 => "quasar telemetry".to_string(),
            2 => format!("quasar notes {}", "extra ".repeat(i % 5)),
            _ => format!("unrelated gardening note {i}"),
        };
        let importance = ((i % 7) as f32) / 10.0 + 0.2;
        let mut input = ThoughtInput::new(ThoughtType::Idea, content).with_importance(importance);
        if i % 16 == 2 && i > 16 {
            input = input.with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::RelatedTo,
                target_id: chain_id(&chain, i - 1),
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]);
        }
        chain.append_thought("agent", input).unwrap();
    }
    chain
}

fn chain_id(chain: &MentisDb, index: usize) -> uuid::Uuid {
    chain.thoughts()[index].id
}

fn indices(result: &mentisdb::RankedSearchResult<'_>) -> Vec<u64> {
    result.hits.iter().map(|hit| hit.thought.index).collect()
}

/// For every limit (including larger than the match count) the returned
/// hits are the prefix of the unlimited ordering and `total_candidates` is the
/// pre-limit match count.
#[test]
fn limited_results_are_prefix_of_full_ordering() {
    let chain = build_chain();
    let base = RankedSearchQuery::new().with_text("quasar telemetry");
    let full = chain.query_ranked(&base.clone().with_limit(10_000));
    let full_order = indices(&full);
    assert_eq!(full_order.len(), 75);
    for limit in [1usize, 2, 5, 10, 33, 100, 299, 300, 301, 10_000] {
        let limited = chain.query_ranked(&base.clone().with_limit(limit));
        assert_eq!(limited.total_candidates, full.total_candidates);
        let want: Vec<u64> = full_order.iter().copied().take(limit).collect();
        assert_eq!(indices(&limited), want, "limit {limit}");
    }
}

/// Heads carry materialized match data and graph paths.
#[test]
fn head_hits_are_fully_materialized() {
    let chain = build_chain();
    let query = RankedSearchQuery::new()
        .with_text("quasar telemetry")
        .with_graph(
            RankedSearchGraph::new()
                .with_max_depth(2)
                .with_max_visited(64)
                .with_mode(GraphExpansionMode::Bidirectional),
        )
        .with_limit(15);
    let result = chain.query_ranked(&query);
    assert_eq!(result.hits.len(), 15);
    for hit in &result.hits {
        if hit.score.lexical > 0.0 {
            assert!(!hit.matched_terms.is_empty());
            assert!(!hit.match_sources.is_empty());
        }
        if hit.graph_distance.is_some() {
            assert!(hit.graph_path.is_some());
        }
    }
    // Scores are non-increasing within the RRF-free ordering.
    let totals: Vec<f32> = result.hits.iter().map(|hit| hit.score.total).collect();
    assert!(totals.windows(2).all(|w| w[0] >= w[1]));
}

/// Results are identical across repeated calls even with many exact ties.
#[test]
fn ordering_is_deterministic_with_ties() {
    let chain = build_chain();
    let query = RankedSearchQuery::new()
        .with_text("quasar telemetry")
        .with_limit(40);
    let first = indices(&chain.query_ranked(&query));
    for _ in 0..5 {
        assert_eq!(indices(&chain.query_ranked(&query)), first);
    }
}

/// Text-free (heuristic) queries also select the correct head.
#[test]
fn heuristic_queries_return_prefix_of_full_ordering() {
    let chain = build_chain();
    let full = indices(&chain.query_ranked(&RankedSearchQuery::new().with_limit(10_000)));
    assert_eq!(full.len(), 400);
    for limit in [1usize, 7, 50, 400] {
        let limited = indices(&chain.query_ranked(&RankedSearchQuery::new().with_limit(limit)));
        assert_eq!(limited, full[..limit].to_vec());
    }
}
