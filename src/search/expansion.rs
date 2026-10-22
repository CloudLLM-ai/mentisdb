use super::{
    AdjacencyDirection, GraphEdge, GraphEdgeProvenance, GraphExpansionHop, GraphExpansionPath,
    ThoughtAdjacencyIndex, ThoughtLocator,
};
use crate::ThoughtRelationKind;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

/// Direction policy for graph expansion during ranked search.
///
/// Controls which direction(s) the graph traversal follows when expanding
/// from lexical seed thoughts using `ThoughtRelation` edges.
///
/// - `OutgoingOnly` — follow "this thought points to other thoughts" (causality, summaries, etc.)
/// - `IncomingOnly` — follow "other thoughts point to this one" (what led here, corrections, etc.)
/// - `Bidirectional` — both directions (most common for broad recall)
///
/// Used inside `RankedSearchGraph`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphExpansionMode {
    /// Expand only along outgoing edges from the current frontier.
    OutgoingOnly,
    /// Expand only along incoming edges into the current frontier.
    IncomingOnly,
    /// Expand both outgoing and incoming edges in a fixed deterministic order.
    Bidirectional,
}

impl GraphExpansionMode {
    fn directions(self) -> &'static [AdjacencyDirection] {
        const OUTGOING: &[AdjacencyDirection] = &[AdjacencyDirection::Outgoing];
        const INCOMING: &[AdjacencyDirection] = &[AdjacencyDirection::Incoming];
        const BIDIRECTIONAL: &[AdjacencyDirection] =
            &[AdjacencyDirection::Outgoing, AdjacencyDirection::Incoming];

        match self {
            Self::OutgoingOnly => OUTGOING,
            Self::IncomingOnly => INCOMING,
            Self::Bidirectional => BIDIRECTIONAL,
        }
    }
}

/// Breadth-first graph expansion request over a prebuilt adjacency snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExpansionQuery {
    /// Seed thoughts to expand from.
    pub seeds: Vec<ThoughtLocator>,
    /// Maximum number of hops from each seed.
    pub max_depth: usize,
    /// Maximum number of unique locators to visit, including seeds.
    pub max_visited: usize,
    /// Whether the result should include the seed nodes as depth-0 hits.
    pub include_seeds: bool,
    /// Direction policy for traversal.
    pub mode: GraphExpansionMode,
}

impl GraphExpansionQuery {
    /// Create a new query from one or more seed locators.
    pub fn new(seeds: Vec<ThoughtLocator>) -> Self {
        Self {
            seeds,
            max_depth: 2,
            max_visited: 128,
            include_seeds: true,
            mode: GraphExpansionMode::Bidirectional,
        }
    }

    /// Set the maximum graph distance explored from each seed.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set the maximum number of unique locators to visit.
    pub fn with_max_visited(mut self, max_visited: usize) -> Self {
        self.max_visited = max_visited.max(1);
        self
    }

    /// Control whether seeds appear as depth-0 hits in the result.
    pub fn with_include_seeds(mut self, include_seeds: bool) -> Self {
        self.include_seeds = include_seeds;
        self
    }

    /// Replace the traversal direction mode.
    pub fn with_mode(mut self, mode: GraphExpansionMode) -> Self {
        self.mode = mode;
        self
    }
}

/// One reached node together with its best discovered provenance path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExpansionHit {
    /// Reached thought locator.
    pub locator: ThoughtLocator,
    /// Shortest discovered distance from the originating seed.
    pub depth: usize,
    /// Provenance path showing how this node was reached.
    pub path: GraphExpansionPath,
}

/// Counters describing one graph-expansion run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphExpansionStats {
    /// Number of unique locators visited, including seeds.
    pub visited_count: usize,
    /// Number of hits emitted in the final result.
    pub emitted_count: usize,
    /// Number of graph edges inspected during expansion.
    pub traversed_edge_count: usize,
    /// Whether expansion stopped early because the visit budget was exhausted.
    pub truncated: bool,
}

/// Deterministic graph-expansion output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphExpansionResult {
    /// Deduplicated reached nodes in discovery order.
    pub hits: Vec<GraphExpansionHit>,
    /// Summary counters for the run.
    pub stats: GraphExpansionStats,
}

impl GraphExpansionResult {
    /// Expand a set of seed nodes over a prebuilt adjacency snapshot.
    pub fn expand(index: &ThoughtAdjacencyIndex, query: &GraphExpansionQuery) -> Self {
        Self::expand_with_overlay(index, query, None, &HashMap::new())
    }

    /// Expand with an optional implicit-edge overlay supplementing explicit relations.
    ///
    /// Every reached node counts against `max_visited` and is emitted as a hit.
    /// See [`GraphExpansionResult::expand_with_overlay_classified`] to count
    /// and emit only a subset of nodes.
    pub fn expand_with_overlay(
        index: &ThoughtAdjacencyIndex,
        query: &GraphExpansionQuery,
        overlay: Option<&crate::search::ImplicitEdgeOverlay>,
        id_lookup: &HashMap<Uuid, ThoughtLocator>,
    ) -> Self {
        Self::expand_with_overlay_classified(index, query, overlay, id_lookup, |_| {
            ExpansionNodeRole::Emit
        })
    }

    /// Expand with an overlay, letting `classify` decide how each reached node
    /// is treated (see [`ExpansionNodeRole`]).
    ///
    /// This is how ranked search keeps its visit budget for nodes that can
    /// actually be returned: nodes excluded by a candidate filter are
    /// [`ExpansionNodeRole::PassThrough`], so they are traversed (they can
    /// connect allowed nodes) but neither count against `max_visited` nor
    /// produce hits. Pass-through nodes are themselves capped at
    /// [`PASS_THROUGH_BUDGET_FACTOR`] times `max_visited` so a filter that
    /// excludes most of a large graph cannot make one expansion walk all of it.
    ///
    /// Paths are stored in a parent-pointer arena and reconstructed only for
    /// emitted hits, so queued nodes cost one small record instead of a full
    /// cloned [`GraphExpansionPath`].
    pub fn expand_with_overlay_classified<F>(
        index: &ThoughtAdjacencyIndex,
        query: &GraphExpansionQuery,
        overlay: Option<&crate::search::ImplicitEdgeOverlay>,
        id_lookup: &HashMap<Uuid, ThoughtLocator>,
        mut classify: F,
    ) -> Self
    where
        F: FnMut(&ThoughtLocator) -> ExpansionNodeRole,
    {
        let mut arena: Vec<ArenaNode<'_>> = Vec::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        let mut stats = GraphExpansionStats::default();
        let mut counted = 0usize;
        let mut passed = 0usize;
        let pass_budget = query.max_visited.saturating_mul(PASS_THROUGH_BUDGET_FACTOR);

        for seed in dedupe_seed_order(&query.seeds) {
            let role = classify(&seed);
            if role.counts() && counted >= query.max_visited {
                stats.truncated = true;
                break;
            }
            if !seen.insert(seed.clone()) {
                continue;
            }
            if role.counts() {
                counted += 1;
            } else {
                passed += 1;
            }
            let id = arena.len();
            if query.include_seeds && role == ExpansionNodeRole::Emit {
                hits.push(GraphExpansionHit {
                    locator: seed.clone(),
                    depth: 0,
                    path: GraphExpansionPath::new(seed.clone()),
                });
            }
            arena.push(ArenaNode {
                parent: None,
                kind: NodeKind::Root(seed),
                depth: 0,
            });
            queue.push_back(id);
        }

        'outer: while let Some(node_id) = queue.pop_front() {
            let depth = arena[node_id].depth;
            if depth >= query.max_depth {
                continue;
            }
            let current = arena[node_id].locator().clone();

            for &direction in query.mode.directions() {
                for edge in index.neighbors(&current, direction) {
                    stats.traversed_edge_count += 1;
                    let next_locator = edge.arrival(direction);
                    if seen.contains(next_locator) {
                        continue;
                    }
                    let role = classify(next_locator);
                    if role.counts() {
                        if counted >= query.max_visited {
                            stats.truncated = true;
                            break 'outer;
                        }
                        counted += 1;
                    } else {
                        if passed >= pass_budget {
                            continue;
                        }
                        passed += 1;
                    }
                    seen.insert(next_locator.clone());
                    let next_id = arena.len();
                    arena.push(ArenaNode {
                        parent: Some(node_id),
                        kind: NodeKind::Hop {
                            direction,
                            edge: Cow::Borrowed(edge),
                        },
                        depth: depth + 1,
                    });
                    if role == ExpansionNodeRole::Emit {
                        hits.push(GraphExpansionHit {
                            locator: next_locator.clone(),
                            depth: depth + 1,
                            path: build_path(&arena, next_id),
                        });
                    }
                    queue.push_back(next_id);
                }
            }

            // Implicit edges from overlay (treated as bidirectional RelatedTo)
            if let Some(overlay) = overlay {
                if let Some(neighbors) = overlay.edges.get(&current.thought_id) {
                    for neighbor in neighbors {
                        let Some(neighbor_locator) = id_lookup.get(&neighbor.thought_id) else {
                            continue;
                        };
                        if seen.contains(neighbor_locator) {
                            continue;
                        }
                        let role = classify(neighbor_locator);
                        if role.counts() {
                            if counted >= query.max_visited {
                                stats.truncated = true;
                                break 'outer;
                            }
                            counted += 1;
                        } else {
                            if passed >= pass_budget {
                                continue;
                            }
                            passed += 1;
                        }

                        let synthetic_edge = GraphEdge {
                            source: current.clone(),
                            target: neighbor_locator.clone(),
                            provenances: vec![GraphEdgeProvenance::Relation {
                                relation_position: 0,
                                kind: ThoughtRelationKind::RelatedTo,
                                chain_key: None,
                            }],
                        };
                        seen.insert(neighbor_locator.clone());
                        let next_id = arena.len();
                        arena.push(ArenaNode {
                            parent: Some(node_id),
                            kind: NodeKind::Hop {
                                direction: AdjacencyDirection::Outgoing,
                                edge: Cow::Owned(synthetic_edge),
                            },
                            depth: depth + 1,
                        });
                        if role == ExpansionNodeRole::Emit {
                            hits.push(GraphExpansionHit {
                                locator: neighbor_locator.clone(),
                                depth: depth + 1,
                                path: build_path(&arena, next_id),
                            });
                        }
                        queue.push_back(next_id);
                    }
                }
            }
        }

        stats.visited_count = seen.len();
        stats.emitted_count = hits.len();
        Self { hits, stats }
    }
}

/// How [`GraphExpansionResult::expand_with_overlay_classified`] treats a
/// reached node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExpansionNodeRole {
    /// Counts against the visit budget and is emitted as a hit.
    Emit,
    /// Counts against the visit budget but is not emitted (for example another
    /// seed that is reported through its own expansion).
    CountOnly,
    /// Traversed through but neither counted nor emitted (for example a node
    /// excluded by a candidate filter). Bounded by
    /// [`PASS_THROUGH_BUDGET_FACTOR`].
    PassThrough,
}

impl ExpansionNodeRole {
    fn counts(self) -> bool {
        !matches!(self, Self::PassThrough)
    }
}

/// Pass-through nodes may number at most this many times `max_visited`.
pub const PASS_THROUGH_BUDGET_FACTOR: usize = 8;

/// One discovered node in the expansion arena.
struct ArenaNode<'a> {
    parent: Option<usize>,
    kind: NodeKind<'a>,
    depth: usize,
}

enum NodeKind<'a> {
    Root(ThoughtLocator),
    Hop {
        direction: AdjacencyDirection,
        edge: Cow<'a, GraphEdge>,
    },
}

impl ArenaNode<'_> {
    fn locator(&self) -> &ThoughtLocator {
        match &self.kind {
            NodeKind::Root(locator) => locator,
            NodeKind::Hop { direction, edge } => edge.arrival(*direction),
        }
    }
}

/// Reconstruct the full provenance path to `node_id` by walking parent
/// pointers back to the seed.
fn build_path(arena: &[ArenaNode<'_>], node_id: usize) -> GraphExpansionPath {
    let mut hops = Vec::with_capacity(arena[node_id].depth);
    let mut cursor = node_id;
    let seed = loop {
        let node = &arena[cursor];
        match &node.kind {
            NodeKind::Root(locator) => break locator.clone(),
            NodeKind::Hop { direction, edge } => {
                hops.push(GraphExpansionHop::new(
                    *direction,
                    edge.clone().into_owned(),
                ));
            }
        }
        match node.parent {
            Some(parent) => cursor = parent,
            None => break node.locator().clone(),
        }
    };
    hops.reverse();
    GraphExpansionPath { seed, hops }
}

fn dedupe_seed_order(seeds: &[ThoughtLocator]) -> Vec<ThoughtLocator> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(seeds.len());
    for seed in seeds {
        if seen.insert(seed.clone()) {
            deduped.push(seed.clone());
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{ImplicitEdgeOverlay, ImplicitNeighbor};
    use uuid::Uuid;

    fn make_locator(id: Uuid, index: u64) -> ThoughtLocator {
        ThoughtLocator {
            chain_key: None,
            thought_id: id,
            thought_index: Some(index),
        }
    }

    #[test]
    fn test_expand_with_overlay_no_explicit_edges() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let loc_a = make_locator(id_a, 0);
        let loc_b = make_locator(id_b, 1);

        let adjacency = ThoughtAdjacencyIndex::from_thoughts(&[]);
        let mut overlay = ImplicitEdgeOverlay::new(0.5, 5);
        overlay.edges.insert(
            id_a,
            vec![ImplicitNeighbor {
                thought_id: id_b,
                cosine_score: 0.9,
            }],
        );

        let mut id_lookup = HashMap::new();
        id_lookup.insert(id_a, loc_a.clone());
        id_lookup.insert(id_b, loc_b.clone());

        let result = GraphExpansionResult::expand_with_overlay(
            &adjacency,
            &GraphExpansionQuery::new(vec![loc_a.clone()]),
            Some(&overlay),
            &id_lookup,
        );

        let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.locator.thought_id).collect();
        assert!(hit_ids.contains(&id_b));
    }

    #[test]
    fn test_expand_overlay_does_not_revisit() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let loc_a = make_locator(id_a, 0);
        let loc_b = make_locator(id_b, 1);

        let adjacency = ThoughtAdjacencyIndex::from_thoughts(&[]);
        let mut overlay = ImplicitEdgeOverlay::new(0.5, 5);
        overlay.edges.insert(
            id_a,
            vec![ImplicitNeighbor {
                thought_id: id_b,
                cosine_score: 0.9,
            }],
        );
        overlay.edges.insert(
            id_b,
            vec![ImplicitNeighbor {
                thought_id: id_a,
                cosine_score: 0.9,
            }],
        );

        let mut id_lookup = HashMap::new();
        id_lookup.insert(id_a, loc_a.clone());
        id_lookup.insert(id_b, loc_b.clone());

        let result = GraphExpansionResult::expand_with_overlay(
            &adjacency,
            &GraphExpansionQuery::new(vec![loc_a.clone()]),
            Some(&overlay),
            &id_lookup,
        );

        let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.locator.thought_id).collect();
        assert_eq!(hit_ids.iter().filter(|&&id| id == id_b).count(), 1);
    }

    #[test]
    fn test_expand_overlay_respects_max_visited() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let id_c = Uuid::new_v4();
        let loc_a = make_locator(id_a, 0);
        let loc_b = make_locator(id_b, 1);
        let loc_c = make_locator(id_c, 2);

        let adjacency = ThoughtAdjacencyIndex::from_thoughts(&[]);
        let mut overlay = ImplicitEdgeOverlay::new(0.5, 5);
        overlay.edges.insert(
            id_a,
            vec![
                ImplicitNeighbor {
                    thought_id: id_b,
                    cosine_score: 0.9,
                },
                ImplicitNeighbor {
                    thought_id: id_c,
                    cosine_score: 0.8,
                },
            ],
        );

        let mut id_lookup = HashMap::new();
        id_lookup.insert(id_a, loc_a.clone());
        id_lookup.insert(id_b, loc_b.clone());
        id_lookup.insert(id_c, loc_c.clone());

        let result = GraphExpansionResult::expand_with_overlay(
            &adjacency,
            &GraphExpansionQuery::new(vec![loc_a.clone()])
                .with_max_visited(2)
                .with_include_seeds(false),
            Some(&overlay),
            &id_lookup,
        );

        assert!(result.stats.truncated);
        assert_eq!(result.stats.visited_count, 2);
    }

    #[test]
    fn test_expand_overlay_adds_hit_beyond_explicit_edges() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let loc_a = make_locator(id_a, 0);
        let loc_b = make_locator(id_b, 1);

        let adjacency = ThoughtAdjacencyIndex::from_thoughts(&[]);
        let mut overlay = ImplicitEdgeOverlay::new(0.5, 5);
        overlay.edges.insert(
            id_a,
            vec![ImplicitNeighbor {
                thought_id: id_b,
                cosine_score: 0.9,
            }],
        );

        let mut id_lookup = HashMap::new();
        id_lookup.insert(id_a, loc_a.clone());
        id_lookup.insert(id_b, loc_b.clone());

        let with_overlay = GraphExpansionResult::expand_with_overlay(
            &adjacency,
            &GraphExpansionQuery::new(vec![loc_a.clone()]),
            Some(&overlay),
            &id_lookup,
        );

        let without_overlay = GraphExpansionResult::expand_with_overlay(
            &adjacency,
            &GraphExpansionQuery::new(vec![loc_a.clone()]),
            None,
            &id_lookup,
        );

        assert_eq!(with_overlay.hits.len(), without_overlay.hits.len() + 1);
    }
}
