use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::lexical::LexicalMatchSource;
use mentisdb::{
    MentisDb, RankedSearchBackend, RankedSearchQuery, ThoughtInput, ThoughtQuery, ThoughtType,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_ranked_query_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn ranked_query_reorders_lexical_matches_without_changing_query_semantics() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-ordering").unwrap();

    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Idea, "Consider vector search later.")
                .with_importance(0.3),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Plan, "Vector search ranking plan.")
                .with_importance(0.8)
                .with_tags(["vector", "search"])
                .with_concepts(["vector search"]),
        )
        .unwrap();

    let filtered = chain.query(&ThoughtQuery::new().with_text("vector search"));
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].content, "Consider vector search later.");
    assert_eq!(filtered[1].content, "Vector search ranking plan.");

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("vector search")
            .with_limit(1),
    );
    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Vector search ranking plan."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_respects_exact_filters_before_lexical_ordering() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-filtered").unwrap();

    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Idea, "Vector search note for later.")
                .with_importance(0.2),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Constraint,
                "Vector search must remain optional.",
            )
            .with_importance(0.9)
            .with_tags(["vector", "search"]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("vector search")
            .with_filter(ThoughtQuery::new().with_types(vec![ThoughtType::Constraint])),
    );
    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Vector search must remain optional."
    );
    assert!(ranked.hits[0].score.lexical > 0.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_without_text_falls_back_to_heuristic_ordering() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-heuristic").unwrap();

    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Insight, "Older but more important.")
                .with_importance(1.0)
                .with_confidence(1.0),
        )
        .unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Insight, "Newer but lower signal.")
                .with_importance(0.1)
                .with_confidence(0.1),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_types(vec![ThoughtType::Insight])),
    );
    assert_eq!(ranked.backend, RankedSearchBackend::Heuristic);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 2);
    assert_eq!(ranked.hits[0].thought.content, "Older but more important.");
    assert_eq!(ranked.hits[1].thought.content, "Newer but lower signal.");
    assert_eq!(ranked.hits[0].score.lexical, 0.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_surfaces_lexical_match_explanations() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-match-explanations").unwrap();

    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Plan,
                "Use BM25 lexical search after the structured filter step.",
            )
            .with_tags(["search"])
            .with_concepts(["bm25"]),
        )
        .unwrap();

    let ranked = chain.query_ranked(&RankedSearchQuery::new().with_text("bm25 search"));

    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(ranked.hits[0].matched_terms, vec!["bm25", "search"]);
    assert!(ranked.hits[0]
        .match_sources
        .contains(&LexicalMatchSource::Content));
    assert!(ranked.hits[0]
        .match_sources
        .contains(&LexicalMatchSource::Tags));
    assert!(ranked.hits[0]
        .match_sources
        .contains(&LexicalMatchSource::Concepts));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_scores_agent_registry_text_lexically() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-agent-registry").unwrap();

    chain
        .upsert_agent(
            "planner",
            Some("Systems Planner"),
            Some("mentisdb"),
            Some("Lexical architect for search quality"),
            None,
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Rebuildable retrieval state matters more than cached prompts.",
            ),
        )
        .unwrap();
    chain
        .append_thought(
            "operator",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Operational dashboards are useful, but not about architecture.",
            ),
        )
        .unwrap();

    let ranked = chain.query_ranked(&RankedSearchQuery::new().with_text("architect"));

    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(ranked.hits[0].thought.agent_id, "planner");
    assert_eq!(ranked.hits[0].matched_terms, vec!["architect"]);
    assert!(ranked.hits[0]
        .match_sources
        .contains(&LexicalMatchSource::AgentRegistry));

    let _ = std::fs::remove_dir_all(&dir);
}
