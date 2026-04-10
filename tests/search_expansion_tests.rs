use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::{
    GraphExpansionMode, GraphExpansionQuery, GraphExpansionResult, ThoughtAdjacencyIndex,
    ThoughtLocator,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType};

static EXPANSION_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = EXPANSION_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_expansion_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn expansion_follows_outgoing_edges_and_preserves_shortest_paths() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-expansion-outgoing").unwrap();

    let base = chain
        .append("astro", ThoughtType::FactLearned, "Seed fact.")
        .unwrap()
        .clone();
    let middle = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Middle summary").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::Supports,
                    target_id: base.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
            ]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Top decision").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: middle.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
            ]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed = adjacency.local_locator_for_index(2).unwrap().clone();
    let result = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![seed.clone()])
            .with_mode(GraphExpansionMode::OutgoingOnly)
            .with_max_depth(2),
    );

    assert_eq!(result.stats.visited_count, 3);
    assert_eq!(result.hits.len(), 3);
    assert_eq!(result.hits[0].locator, seed);
    assert_eq!(
        result.hits[1].locator,
        adjacency.local_locator_for_index(1).unwrap().clone()
    );
    assert_eq!(result.hits[1].depth, 1);
    assert_eq!(
        result.hits[1]
            .path
            .visited()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            adjacency.local_locator_for_index(2).unwrap().clone(),
            adjacency.local_locator_for_index(1).unwrap().clone(),
        ]
    );
    assert_eq!(
        result.hits[2].locator,
        adjacency.local_locator_for_index(0).unwrap().clone()
    );
    assert_eq!(result.hits[2].depth, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expansion_can_follow_incoming_only_without_emitting_seed_hits() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-expansion-incoming").unwrap();

    let seed = chain
        .append("astro", ThoughtType::Finding, "Original seed.")
        .unwrap()
        .clone();
    let derived = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Hypothesis, "Derived thought").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: seed.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
            ]),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Plan, "Follow-up thought").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::ContinuesFrom,
                    target_id: derived.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
            ]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let result = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![ThoughtLocator::local(&seed)])
            .with_mode(GraphExpansionMode::IncomingOnly)
            .with_include_seeds(false)
            .with_max_depth(2),
    );

    assert_eq!(result.hits.len(), 2);
    assert_eq!(result.hits[0].locator, ThoughtLocator::local(&derived));
    assert_eq!(result.hits[0].depth, 1);
    assert_eq!(
        result.hits[1].locator,
        adjacency.local_locator_for_index(2).unwrap().clone()
    );
    assert_eq!(result.hits[1].depth, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expansion_marks_truncation_when_visit_budget_is_exhausted() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-expansion-budget").unwrap();

    let seed = chain
        .append("astro", ThoughtType::Finding, "Seed")
        .unwrap()
        .clone();
    for idx in 0..3 {
        chain
            .append_thought(
                "astro",
                ThoughtInput::new(ThoughtType::Idea, format!("Neighbor {idx}")).with_relations(
                    vec![ThoughtRelation {
                        kind: ThoughtRelationKind::DerivedFrom,
                        target_id: seed.id,
                        chain_key: None,
                        valid_at: None,
                        invalid_at: None,
                    }],
                ),
            )
            .unwrap();
    }

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let result = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![ThoughtLocator::local(&seed)])
            .with_mode(GraphExpansionMode::IncomingOnly)
            .with_max_depth(1)
            .with_max_visited(2),
    );

    assert!(result.stats.truncated);
    assert_eq!(result.stats.visited_count, 2);
    assert_eq!(result.hits.len(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expansion_dedupes_duplicate_seeds_and_keeps_first_seed_path() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-expansion-multiseed").unwrap();

    let left = chain
        .append("astro", ThoughtType::Finding, "Left seed")
        .unwrap()
        .clone();
    let right = chain
        .append("astro", ThoughtType::Finding, "Right seed")
        .unwrap()
        .clone();
    let bridge = chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Bridge").with_relations(vec![
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: left.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
                ThoughtRelation {
                    kind: ThoughtRelationKind::DerivedFrom,
                    target_id: right.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                },
            ]),
        )
        .unwrap()
        .clone();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let result = GraphExpansionResult::expand(
        &adjacency,
        &GraphExpansionQuery::new(vec![
            ThoughtLocator::local(&left),
            ThoughtLocator::local(&left),
            ThoughtLocator::local(&right),
        ])
        .with_mode(GraphExpansionMode::IncomingOnly)
        .with_include_seeds(false)
        .with_max_depth(1),
    );

    assert_eq!(result.stats.visited_count, 3);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].locator, ThoughtLocator::local(&bridge));
    assert_eq!(result.hits[0].path.seed, ThoughtLocator::local(&left));

    let _ = std::fs::remove_dir_all(&dir);
}
