//! Tests for classified graph expansion: budget accounting, pass-through
//! traversal, and arena path reconstruction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::{
    ExpansionNodeRole, GraphExpansionMode, GraphExpansionQuery, GraphExpansionResult,
    ThoughtAdjacencyIndex, ThoughtLocator, PASS_THROUGH_BUDGET_FACTOR,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Linear chain where thought `i` references thought `i - 1`.
fn linear_chain(len: usize) -> MentisDb {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_expansion_classified_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain = MentisDb::open_with_key(&dir, "expansion").unwrap();
    for i in 0..len {
        let mut input = ThoughtInput::new(ThoughtType::Idea, format!("node {i}"));
        if i > 0 {
            input = input.with_refs(vec![(i - 1) as u64]);
        }
        chain.append_thought("a", input).unwrap();
    }
    chain
}

fn index_of(locator: &ThoughtLocator) -> u64 {
    locator.thought_index.unwrap()
}

/// Plain expansion keeps its documented behaviour: BFS order, depths, and
/// full provenance paths reconstructed from the arena.
#[test]
fn plain_expansion_reconstructs_full_paths() {
    let chain = linear_chain(6);
    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed = ThoughtLocator::local(&chain.thoughts()[0]);
    let result = GraphExpansionResult::expand_with_overlay(
        &adjacency,
        &GraphExpansionQuery::new(vec![seed.clone()])
            .with_mode(GraphExpansionMode::IncomingOnly)
            .with_max_depth(3),
        None,
        &HashMap::new(),
    );
    let indices: Vec<u64> = result.hits.iter().map(|h| index_of(&h.locator)).collect();
    assert_eq!(indices, vec![0, 1, 2, 3]);
    let last = result.hits.last().unwrap();
    assert_eq!(last.depth, 3);
    assert_eq!(last.path.seed, seed);
    assert_eq!(last.path.hops.len(), 3);
    let visited: Vec<u64> = last.path.visited().into_iter().map(index_of).collect();
    assert_eq!(visited, vec![0, 1, 2, 3]);
    assert_eq!(result.stats.visited_count, 4);
    assert_eq!(result.stats.emitted_count, 4);
}

/// Excluded (pass-through) nodes are traversed but do not spend the visit
/// budget, so allowed nodes beyond them are still reached.
#[test]
fn pass_through_nodes_do_not_consume_budget() {
    let chain = linear_chain(8);
    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed = ThoughtLocator::local(&chain.thoughts()[0]);
    let query = GraphExpansionQuery::new(vec![seed])
        .with_mode(GraphExpansionMode::IncomingOnly)
        .with_max_depth(10)
        .with_max_visited(3);
    let excluded = |locator: &ThoughtLocator| {
        if index_of(locator) == 1 {
            ExpansionNodeRole::PassThrough
        } else {
            ExpansionNodeRole::Emit
        }
    };
    let result = GraphExpansionResult::expand_with_overlay_classified(
        &adjacency,
        &query,
        None,
        &HashMap::new(),
        excluded,
    );
    let indices: Vec<u64> = result.hits.iter().map(|h| index_of(&h.locator)).collect();
    // Budget 3 = seed + nodes 2 and 3; node 1 is excluded but still traversed.
    assert_eq!(indices, vec![0, 2, 3]);
    assert!(result.stats.truncated);
    let node3 = result.hits.last().unwrap();
    assert_eq!(
        node3.path.hops.len(),
        3,
        "path runs through the excluded node"
    );

    // Without classification the budget is spent on node 1.
    let plain =
        GraphExpansionResult::expand_with_overlay(&adjacency, &query, None, &HashMap::new());
    let plain_indices: Vec<u64> = plain.hits.iter().map(|h| index_of(&h.locator)).collect();
    assert_eq!(plain_indices, vec![0, 1, 2]);
}

/// Count-only nodes spend budget but are never emitted.
#[test]
fn count_only_nodes_spend_budget_without_being_emitted() {
    let chain = linear_chain(8);
    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed = ThoughtLocator::local(&chain.thoughts()[0]);
    let query = GraphExpansionQuery::new(vec![seed])
        .with_mode(GraphExpansionMode::IncomingOnly)
        .with_max_depth(10)
        .with_max_visited(3);
    let result = GraphExpansionResult::expand_with_overlay_classified(
        &adjacency,
        &query,
        None,
        &HashMap::new(),
        |locator| {
            if index_of(locator) == 1 {
                ExpansionNodeRole::CountOnly
            } else {
                ExpansionNodeRole::Emit
            }
        },
    );
    let indices: Vec<u64> = result.hits.iter().map(|h| index_of(&h.locator)).collect();
    assert_eq!(indices, vec![0, 2]);
}

/// A filter that excludes everything cannot make one expansion walk the whole graph.
#[test]
fn pass_through_traversal_is_bounded() {
    let chain = linear_chain(60);
    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed = ThoughtLocator::local(&chain.thoughts()[0]);
    let query = GraphExpansionQuery::new(vec![seed])
        .with_mode(GraphExpansionMode::IncomingOnly)
        .with_max_depth(100)
        .with_max_visited(1);
    let result = GraphExpansionResult::expand_with_overlay_classified(
        &adjacency,
        &query,
        None,
        &HashMap::new(),
        |locator| {
            if index_of(locator) == 0 {
                ExpansionNodeRole::Emit
            } else {
                ExpansionNodeRole::PassThrough
            }
        },
    );
    assert_eq!(result.hits.len(), 1);
    assert!(result.stats.visited_count <= 1 + PASS_THROUGH_BUDGET_FACTOR);
}
