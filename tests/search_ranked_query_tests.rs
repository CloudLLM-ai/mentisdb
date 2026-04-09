use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::{lexical::LexicalMatchSource, GraphExpansionMode, ThoughtLocator};
use mentisdb::{
    MentisDb, RankedSearchBackend, RankedSearchGraph, RankedSearchQuery, ThoughtInput,
    ThoughtQuery, ThoughtRelation, ThoughtRelationKind, ThoughtType,
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

#[test]
fn ranked_query_with_graph_expansion_surfaces_supporting_context() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-context").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking seed for retrieval planning.",
            )
            .with_tags(["search"])
            .with_importance(0.9),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Operator rollout checklist for the retrieval launch.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("latency ranking")
            .with_graph(
                RankedSearchGraph::new()
                    .with_max_depth(1)
                    .with_mode(GraphExpansionMode::Bidirectional),
            ),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 2);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Latency ranking seed for retrieval planning."
    );

    let supporting = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Operator rollout checklist for the retrieval launch.")
        .unwrap();
    assert_eq!(supporting.graph_distance, Some(1));
    assert!(supporting.graph_path.is_some());
    assert!(supporting.score.graph > 0.0);
    assert!(supporting.matched_terms.is_empty());
    assert!(supporting.match_sources.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_graph_expansion_stays_inside_filtered_candidates() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-filter").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Latency ranking seed.").with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Unfiltered rollout note that points to the seed.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("latency ranking")
            .with_graph(RankedSearchGraph::new().with_max_depth(1)),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(ranked.hits[0].thought.content, "Latency ranking seed.");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_without_text_ignores_graph_configuration() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-without-text").unwrap();

    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Insight, "Older high-signal note.")
                .with_importance(1.0)
                .with_confidence(1.0)
                .with_tags(["search"]),
        )
        .unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Insight, "Newer low-signal note.")
                .with_importance(0.1)
                .with_confidence(0.1)
                .with_tags(["search"]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_graph(RankedSearchGraph::new().with_max_depth(1)),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::Heuristic);
    assert_eq!(ranked.total_candidates, 2);
    assert!(ranked.hits.iter().all(|hit| hit.graph_distance.is_none()));
    assert!(ranked.hits.iter().all(|hit| hit.graph_path.is_none()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_graph_expansion_prefers_closer_supporting_context() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-depth-order").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking seed for graph-aware retrieval.",
            )
            .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let first_hop = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "First-hop support note for rollout preparation.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Plan,
                "Second-hop support note for operator coordination.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::ContinuesFrom,
                target_id: first_hop.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("latency ranking")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(2),
            ),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 3);
    assert_eq!(ranked.hits.len(), 3);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Latency ranking seed for graph-aware retrieval."
    );
    assert_eq!(
        ranked.hits[1].thought.content,
        "First-hop support note for rollout preparation."
    );
    assert_eq!(
        ranked.hits[2].thought.content,
        "Second-hop support note for operator coordination."
    );
    assert_eq!(ranked.hits[1].graph_distance, Some(1));
    assert_eq!(ranked.hits[2].graph_distance, Some(2));
    assert!(ranked.hits[1].score.graph > ranked.hits[2].score.graph);

    let visited = ranked.hits[2]
        .graph_path
        .as_ref()
        .unwrap()
        .visited()
        .into_iter()
        .map(|locator| locator.thought_index)
        .collect::<Vec<_>>();
    assert_eq!(visited, vec![Some(0), Some(1), Some(2)]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_graph_expansion_can_surface_ref_backlinks() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-refs").unwrap();

    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Retry budget search seed for backlink tests.",
            )
            .with_tags(["search"]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Operational note with only a raw back-reference.",
            )
            .with_tags(["search"])
            .with_refs(vec![0]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("retry budget")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            ),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 2);

    let backlink = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Operational note with only a raw back-reference.")
        .unwrap();
    assert_eq!(backlink.graph_distance, Some(1));
    assert!(backlink.graph_path.is_some());
    assert_eq!(backlink.matched_terms, Vec::<String>::new());
    assert_eq!(
        backlink
            .graph_path
            .as_ref()
            .unwrap()
            .visited()
            .into_iter()
            .map(|locator| locator.thought_index)
            .collect::<Vec<_>>(),
        vec![Some(0), Some(1)]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_graph_without_seed_hits_still_keeps_lexical_match() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-no-seed-hits").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Incident memory seed for explicit include_seeds behavior.",
            )
            .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Follow-up note reached only through graph expansion.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("incident memory")
            .with_graph(
                RankedSearchGraph::new()
                    .with_include_seeds(false)
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            ),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 2);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Incident memory seed for explicit include_seeds behavior."
    );
    assert!(ranked.hits[0].score.lexical > 0.0);
    assert_eq!(ranked.hits[0].graph_distance, None);
    assert!(ranked.hits[0].graph_path.is_none());
    assert_eq!(ranked.hits[1].graph_distance, Some(1));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_relation_weighting_prefers_derived_context_over_related_context() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-relation-weighting").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking seed for relation weighting.",
            )
            .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Derived support context should appear first.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Related support context should appear after derived.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::RelatedTo,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("latency ranking")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            ),
    );

    let derived_index = ranked
        .hits
        .iter()
        .position(|hit| hit.thought.content == "Derived support context should appear first.")
        .unwrap();
    let related_index = ranked
        .hits
        .iter()
        .position(|hit| {
            hit.thought.content == "Related support context should appear after derived."
        })
        .unwrap();
    let derived = &ranked.hits[derived_index];
    let related = &ranked.hits[related_index];

    assert!(derived_index < related_index);
    assert_eq!(derived.graph_distance, Some(1));
    assert_eq!(related.graph_distance, Some(1));
    assert!(derived.score.relation > related.score.relation);
    assert!(derived
        .graph_relation_kinds
        .contains(&ThoughtRelationKind::DerivedFrom));
    assert!(related
        .graph_relation_kinds
        .contains(&ThoughtRelationKind::RelatedTo));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_multi_seed_support_counts_distinct_seed_paths() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-multi-seed-support").unwrap();

    let seed_a = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Memory cap seed alpha.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let seed_b = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Memory cap seed beta.").with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Shared support context reached from both seeds.",
            )
            .with_tags(["search"])
            .with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                },
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_b.id,
                    chain_key: None,
                },
            ]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("memory cap")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            ),
    );

    let shared = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Shared support context reached from both seeds.")
        .unwrap();
    assert_eq!(shared.graph_distance, Some(1));
    assert_eq!(shared.graph_seed_paths, 2);
    assert!(shared.score.seed_support > 0.0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_context_bundles_group_support_without_cross_seed_leakage() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "query-context-bundles").unwrap();

    let seed_a = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Incident memory seed alpha.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let seed_b = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Incident memory seed beta.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Summary, "Alpha-only support note.")
                .with_tags(["search"])
                .with_relations(vec![ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                }]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Summary, "Beta-only support note.")
                .with_tags(["search"])
                .with_relations(vec![ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_b.id,
                    chain_key: None,
                }]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Summary, "Shared support note.")
                .with_tags(["search"])
                .with_relations(vec![
                    ThoughtRelation {
                        kind: ThoughtRelationKind::DerivedFrom,
                        target_id: seed_a.id,
                        chain_key: None,
                    },
                    ThoughtRelation {
                        kind: ThoughtRelationKind::DerivedFrom,
                        target_id: seed_b.id,
                        chain_key: None,
                    },
                ]),
        )
        .unwrap();

    let bundles = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("incident memory")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            )
            .with_limit(2),
    );

    assert_eq!(bundles.bundles.len(), 2);
    let alpha_bundle = &bundles.bundles[0];
    let beta_bundle = &bundles.bundles[1];
    assert_eq!(alpha_bundle.seed.locator, ThoughtLocator::local(&seed_a));
    assert_eq!(beta_bundle.seed.locator, ThoughtLocator::local(&seed_b));
    assert!(alpha_bundle
        .support
        .iter()
        .any(|hit| hit.locator.thought_index == Some(2)));
    assert!(alpha_bundle
        .support
        .iter()
        .any(|hit| hit.locator.thought_index == Some(4)));
    assert!(!alpha_bundle
        .support
        .iter()
        .any(|hit| hit.locator == ThoughtLocator::local(&seed_b)));
    assert!(beta_bundle
        .support
        .iter()
        .any(|hit| hit.locator.thought_index == Some(3)));
    assert!(beta_bundle
        .support
        .iter()
        .any(|hit| hit.locator.thought_index == Some(4)));
    assert!(!beta_bundle
        .support
        .iter()
        .any(|hit| hit.locator == ThoughtLocator::local(&seed_a)));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_query_graph_tracks_multiple_supporting_seeds() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-query-graph-multi-seed").unwrap();

    let seed_a = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Latency ranking seed.").with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let seed_b = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Security ranking seed.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Shared rollout context reachable from both origins.",
            )
            .with_tags(["search"])
            .with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                },
                ThoughtRelation {
                    kind: ThoughtRelationKind::ContinuesFrom,
                    target_id: seed_b.id,
                    chain_key: None,
                },
            ]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("ranking seed")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            ),
    );

    let shared = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Shared rollout context reachable from both origins.")
        .unwrap();
    assert_eq!(shared.graph_seed_paths, 2);
    assert!(shared.score.seed_support > 0.0);
    assert!(shared
        .graph_relation_kinds
        .contains(&ThoughtRelationKind::DerivedFrom));
    assert!(shared
        .graph_relation_kinds
        .contains(&ThoughtRelationKind::ContinuesFrom));

    let _ = std::fs::remove_dir_all(&dir);
}
