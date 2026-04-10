use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::GraphExpansionMode;
use mentisdb::{
    MentisDb, RankedSearchBackend, RankedSearchGraph, RankedSearchQuery, ThoughtInput,
    ThoughtQuery, ThoughtRelation, ThoughtRelationKind, ThoughtType,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_ranked_eval_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn ranked_search_filter_remains_authoritative_before_lexical_ranking() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-filter-authority-eval").unwrap();

    chain
        .append_thought(
            "planner-a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Navigator lexical signal should survive filtering.",
            )
            .with_tags(["search", "keep"]),
        )
        .unwrap();

    chain
        .append_thought(
            "planner-b",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Navigator lexical signal should be filtered out first.",
            )
            .with_tags(["archive"]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("navigator lexical"),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 1);
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(ranked.hits[0].thought.agent_id, "planner-a");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_search_blank_text_falls_back_to_heuristic_without_dropping_candidates() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-blank-text-eval").unwrap();

    chain
        .append_thought(
            "agent",
            ThoughtInput::new(
                ThoughtType::Insight,
                "Older high-signal ranked search note.",
            )
            .with_importance(1.0)
            .with_confidence(1.0)
            .with_tags(["search"]),
        )
        .unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(
                ThoughtType::Insight,
                "Newer lower-signal ranked search note.",
            )
            .with_importance(0.1)
            .with_confidence(0.1)
            .with_tags(["search"]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("   ")
            .with_limit(10),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::Heuristic);
    assert_eq!(ranked.total_candidates, 2);
    assert_eq!(ranked.hits.len(), 2);
    assert_eq!(
        ranked.hits[0].thought.content,
        "Older high-signal ranked search note."
    );
    assert_eq!(
        ranked.hits[1].thought.content,
        "Newer lower-signal ranked search note."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_search_reports_pre_limit_candidate_count_and_truncates_after_ranking() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-limit-eval").unwrap();

    for (i, importance) in [0.0_f32, 1.0, 0.5, 0.1].into_iter().enumerate() {
        chain
            .append_thought(
                "agent",
                ThoughtInput::new(
                    ThoughtType::Decision,
                    format!("ranked lexical candidate {i} with shared tokens"),
                )
                .with_tags(["search", "ranked"])
                .with_importance(importance),
            )
            .unwrap();
    }

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["ranked"]))
            .with_text("ranked lexical candidate")
            .with_limit(2),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::Lexical);
    assert_eq!(ranked.total_candidates, 4);
    assert_eq!(ranked.hits.len(), 2);
    assert!(ranked.hits[0].score.total >= ranked.hits[1].score.total);
    assert_eq!(
        ranked.hits[0].thought.content,
        "ranked lexical candidate 1 with shared tokens"
    );
    assert_eq!(
        ranked.hits[1].thought.content,
        "ranked lexical candidate 2 with shared tokens"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_search_graph_paths_expose_relation_kind_provenance_for_future_weighting() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-relation-provenance-eval").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking seed for provenance-aware retrieval.",
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
                "Derived support node should keep relation provenance.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Related support node should keep relation provenance.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::RelatedTo,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
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

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);

    let derived = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Derived support node should keep relation provenance.")
        .unwrap();
    let related = ranked
        .hits
        .iter()
        .find(|hit| hit.thought.content == "Related support node should keep relation provenance.")
        .unwrap();

    assert_eq!(derived.graph_distance, Some(1));
    assert_eq!(related.graph_distance, Some(1));
    assert!(derived.score.graph > 0.0);
    assert_eq!(derived.score.graph, related.score.graph);

    let derived_kinds: HashSet<ThoughtRelationKind> = derived
        .graph_path
        .as_ref()
        .unwrap()
        .hops
        .iter()
        .flat_map(|hop| hop.edge.provenances.iter())
        .filter_map(|provenance| match provenance {
            mentisdb::search::GraphEdgeProvenance::Relation { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    let related_kinds: HashSet<ThoughtRelationKind> = related
        .graph_path
        .as_ref()
        .unwrap()
        .hops
        .iter()
        .flat_map(|hop| hop.edge.provenances.iter())
        .filter_map(|provenance| match provenance {
            mentisdb::search::GraphEdgeProvenance::Relation { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();

    assert!(derived_kinds.contains(&ThoughtRelationKind::DerivedFrom));
    assert!(related_kinds.contains(&ThoughtRelationKind::RelatedTo));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_search_graph_multi_seed_shared_context_is_deduped_with_stable_seed_path() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-multi-seed-shared-context-eval").unwrap();

    let seed_a = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Shared memory-cap seed alpha.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let seed_b = chain
        .append_thought(
            "planner",
            ThoughtInput::new(ThoughtType::Decision, "Shared memory-cap seed beta.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Common support context linked to both lexical seeds.",
            )
            .with_tags(["search"])
            .with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_b.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
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

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);
    assert_eq!(ranked.total_candidates, 3);
    assert_eq!(ranked.hits.len(), 3);

    let shared = ranked
        .hits
        .iter()
        .filter(|hit| hit.thought.content == "Common support context linked to both lexical seeds.")
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].graph_distance, Some(1));
    assert!(shared[0].matched_terms.is_empty());
    let seed_id = shared[0].graph_path.as_ref().unwrap().seed.thought_id;
    assert!(seed_id == seed_a.id || seed_id == seed_b.id);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranked_search_graph_results_support_seed_grouped_context_bundles() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "ranked-bundle-shape-eval").unwrap();

    let seed = chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Incident memory seed for grouped context retrieval.",
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
                "Context node A reachable through relation graph.",
            )
            .with_tags(["search"])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();
    chain
        .append_thought(
            "planner",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Context node B reachable through ref backlink.",
            )
            .with_tags(["search"])
            .with_refs(vec![0]),
        )
        .unwrap();

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("incident memory")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_include_seeds(false)
                    .with_max_depth(1),
            ),
    );

    assert_eq!(ranked.backend, RankedSearchBackend::LexicalGraph);

    let mut bundles: BTreeMap<uuid::Uuid, Vec<String>> = BTreeMap::new();
    for hit in ranked
        .hits
        .iter()
        .filter(|hit| hit.graph_distance.is_some())
    {
        let seed_id = hit.graph_path.as_ref().unwrap().seed.thought_id;
        bundles
            .entry(seed_id)
            .or_default()
            .push(hit.thought.content.clone());
    }

    assert_eq!(bundles.len(), 1);
    let grouped = bundles.get(&seed.id).unwrap();
    assert_eq!(grouped.len(), 2);
    assert!(grouped
        .iter()
        .any(|content| content == "Context node A reachable through relation graph."));
    assert!(grouped
        .iter()
        .any(|content| content == "Context node B reachable through ref backlink."));

    let _ = std::fs::remove_dir_all(&dir);
}
