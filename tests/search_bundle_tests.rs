use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::{
    build_context_bundles, ContextBundleOptions, ContextBundleSeed, GraphExpansionMode,
    GraphExpansionQuery, GraphExpansionResult, ThoughtAdjacencyIndex, ThoughtLocator,
};
use mentisdb::{
    MentisDb, RankedSearchGraph, RankedSearchQuery, ThoughtInput, ThoughtQuery, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};

static BUNDLE_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = BUNDLE_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_bundle_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn bundles_group_support_by_seed_and_keep_deterministic_order() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-bundle-grouping").unwrap();

    let seed_a = chain
        .append("astro", ThoughtType::Decision, "Latency ranking seed.")
        .unwrap()
        .clone();
    let seed_b = chain
        .append("astro", ThoughtType::Decision, "Security audit seed.")
        .unwrap()
        .clone();
    let a_depth_1 = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "A-first supporting note.").with_relations(
                vec![ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                }],
            ),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Plan, "A-second supporting note.").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::ContinuesFrom,
                    target_id: a_depth_1.id,
                    chain_key: None,
                },
            ]),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "B supporting note.").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed_b.id,
                    chain_key: None,
                },
            ]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let expansion = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![
            ThoughtLocator::local(&seed_b),
            ThoughtLocator::local(&seed_a),
        ])
        .with_mode(GraphExpansionMode::IncomingOnly)
        .with_max_depth(2),
    );
    let seeds = vec![
        ContextBundleSeed::new(ThoughtLocator::local(&seed_a), 9.0)
            .with_matched_terms(["latency", "ranking"]),
        ContextBundleSeed::new(ThoughtLocator::local(&seed_b), 8.0)
            .with_matched_terms(["security", "audit"]),
    ];
    let bundled = build_context_bundles(&seeds, &expansion.hits, ContextBundleOptions::default());

    assert_eq!(bundled.bundles.len(), 2);
    assert_eq!(
        bundled.bundles[0].seed.locator,
        ThoughtLocator::local(&seed_a)
    );
    assert_eq!(
        bundled.bundles[1].seed.locator,
        ThoughtLocator::local(&seed_b)
    );

    let a_support = &bundled.bundles[0].support;
    assert_eq!(a_support.len(), 2);
    assert_eq!(a_support[0].locator, ThoughtLocator::local(&a_depth_1));
    assert_eq!(a_support[0].depth, 1);
    assert_eq!(a_support[1].locator.thought_index, Some(3));
    assert_eq!(a_support[1].depth, 2);

    let b_support = &bundled.bundles[1].support;
    assert_eq!(b_support.len(), 1);
    assert_eq!(b_support[0].locator.thought_index, Some(4));
    assert_eq!(b_support[0].depth, 1);
    assert!(b_support[0]
        .relation_kinds
        .contains(&ThoughtRelationKind::DerivedFrom));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_optionally_include_seed_hits() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-bundle-seeds").unwrap();

    let seed = chain
        .append(
            "astro",
            ThoughtType::Decision,
            "Seed for include-seed test.",
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Supporting context.").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed.id,
                    chain_key: None,
                },
            ]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let expansion = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![ThoughtLocator::local(&seed)])
            .with_mode(GraphExpansionMode::IncomingOnly)
            .with_max_depth(1),
    );
    let seeds = vec![ContextBundleSeed::new(ThoughtLocator::local(&seed), 7.0)];

    let without_seed_hits =
        build_context_bundles(&seeds, &expansion.hits, ContextBundleOptions::default());
    assert_eq!(without_seed_hits.bundles[0].support.len(), 1);
    assert_eq!(without_seed_hits.bundles[0].support[0].depth, 1);

    let with_seed_hits = build_context_bundles(
        &seeds,
        &expansion.hits,
        ContextBundleOptions::new().with_include_seed_hits(true),
    );
    assert_eq!(with_seed_hits.bundles[0].support.len(), 2);
    assert_eq!(with_seed_hits.bundles[0].support[0].depth, 0);
    assert_eq!(
        with_seed_hits.bundles[0].support[0].locator,
        ThoughtLocator::local(&seed)
    );
    assert_eq!(with_seed_hits.bundles[0].support[1].depth, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bundles_collapse_duplicate_hits_per_seed_and_track_path_count() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-bundle-dedup").unwrap();

    let seed = chain
        .append(
            "astro",
            ThoughtType::Decision,
            "Seed for duplicate-hit test.",
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Supporting context for duplicate-hit test.",
            )
            .with_refs(vec![0])
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
            }]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let expansion = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![ThoughtLocator::local(&seed)])
            .with_mode(GraphExpansionMode::IncomingOnly)
            .with_max_depth(1),
    );
    let support_hit = expansion
        .hits
        .iter()
        .find(|hit| hit.depth == 1)
        .unwrap()
        .clone();
    let mut duplicated_hits = expansion.hits.clone();
    duplicated_hits.push(support_hit);
    let seeds = vec![ContextBundleSeed::new(ThoughtLocator::local(&seed), 6.0)];
    let bundled = build_context_bundles(&seeds, &duplicated_hits, ContextBundleOptions::default());

    assert_eq!(bundled.bundles.len(), 1);
    assert_eq!(bundled.bundles[0].support.len(), 1);
    assert_eq!(bundled.bundles[0].support[0].seed_path_count, 2);
    assert!(bundled.bundles[0].support[0]
        .relation_kinds
        .contains(&ThoughtRelationKind::DerivedFrom));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_context_bundles_groups_supporting_context_by_lexical_seed() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-bundle-query-api").unwrap();

    let seed_a = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Latency ranking seed.").with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let seed_b = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Security ranking seed.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Shared rollout context.")
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
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Plan, "Latency-only context.")
                .with_tags(["search"])
                .with_relations(vec![ThoughtRelation {
                    kind: ThoughtRelationKind::ContinuesFrom,
                    target_id: seed_a.id,
                    chain_key: None,
                }]),
        )
        .unwrap();

    let bundles = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("ranking seed")
            .with_graph(
                RankedSearchGraph::new()
                    .with_mode(GraphExpansionMode::IncomingOnly)
                    .with_max_depth(1),
            )
            .with_limit(2),
    );

    assert_eq!(bundles.bundles.len(), 2);
    assert_eq!(
        bundles.bundles[0].seed.locator,
        ThoughtLocator::local(&seed_a)
    );
    assert_eq!(
        bundles.bundles[1].seed.locator,
        ThoughtLocator::local(&seed_b)
    );

    let a_support = &bundles.bundles[0].support;
    assert_eq!(a_support.len(), 2);
    assert_eq!(a_support[0].locator.thought_index, Some(2));
    assert_eq!(a_support[1].locator.thought_index, Some(3));

    let b_support = &bundles.bundles[1].support;
    assert_eq!(b_support.len(), 1);
    assert_eq!(b_support[0].locator.thought_index, Some(2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_context_bundles_without_graph_keeps_seed_order_and_empty_support() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-bundle-no-graph").unwrap();

    let first = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Latency ranking seed.").with_tags(["search"]),
        )
        .unwrap()
        .clone();
    let second = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Security ranking seed.")
                .with_tags(["search"]),
        )
        .unwrap()
        .clone();

    let bundles = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_tags_any(["search"]))
            .with_text("ranking seed")
            .with_limit(2),
    );

    assert_eq!(bundles.bundles.len(), 2);
    assert_eq!(
        bundles.bundles[0].seed.locator,
        ThoughtLocator::local(&first)
    );
    assert_eq!(
        bundles.bundles[1].seed.locator,
        ThoughtLocator::local(&second)
    );
    assert!(bundles
        .bundles
        .iter()
        .all(|bundle| bundle.support.is_empty()));

    let _ = std::fs::remove_dir_all(&dir);
}
