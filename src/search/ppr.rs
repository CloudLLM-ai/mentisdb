use super::{GraphExpansionPath, ThoughtLocator};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// Configuration for personalized PageRank graph expansion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PprConfig {
    /// Probability of following graph edges instead of teleporting back to the
    /// seed distribution.
    pub damping: f32,
    /// Maximum number of power-iteration rounds.
    pub max_iters: usize,
    /// Stop once total score movement between iterations is at or below this.
    pub tolerance: f32,
    /// Maximum number of nodes included in the local PPR graph.
    pub max_nodes: usize,
    /// Whether implicit edges should contribute to the graph walk.
    pub include_implicit_edges: bool,
}

impl Default for PprConfig {
    fn default() -> Self {
        Self {
            damping: 0.85,
            max_iters: 20,
            tolerance: 0.0001,
            max_nodes: 128,
            include_implicit_edges: true,
        }
    }
}

/// One weighted directed edge in the PPR graph view.
#[derive(Debug, Clone, PartialEq)]
pub struct PprEdge {
    /// Edge destination.
    pub target: ThoughtLocator,
    /// Non-negative edge weight. Zero and non-finite weights are ignored.
    pub weight: f32,
    /// Whether this edge came from an implicit source such as vector cosine
    /// similarity instead of an explicit thought relation.
    pub implicit: bool,
}

impl PprEdge {
    /// Build an explicit weighted edge.
    pub fn explicit(target: ThoughtLocator, weight: f32) -> Self {
        Self {
            target,
            weight,
            implicit: false,
        }
    }

    /// Build an implicit weighted edge.
    pub fn implicit(target: ThoughtLocator, weight: f32) -> Self {
        Self {
            target,
            weight,
            implicit: true,
        }
    }
}

/// Weighted adjacency input for PPR expansion.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PprGraph {
    edges: BTreeMap<ThoughtLocator, Vec<PprEdge>>,
}

impl PprGraph {
    /// Create an empty weighted graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node even if it has no outgoing edges.
    pub fn add_node(&mut self, node: ThoughtLocator) {
        self.edges.entry(node).or_default();
    }

    /// Add one directed weighted edge.
    pub fn add_edge(&mut self, source: ThoughtLocator, edge: PprEdge) {
        self.edges.entry(edge.target.clone()).or_default();
        self.edges.entry(source).or_default().push(edge);
    }

    /// Return outgoing weighted edges for a node.
    pub fn outgoing(&self, node: &ThoughtLocator) -> &[PprEdge] {
        self.edges.get(node).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Personalized PageRank result for a bounded local graph.
#[derive(Debug, Clone, PartialEq)]
pub struct PprResult {
    /// Final scores keyed by thought locator.
    pub scores: HashMap<ThoughtLocator, f32>,
    /// Optional provenance paths from seeds. The isolated PPR graph input does
    /// not require callers to provide these, but the field is ready for graph
    /// expansion wiring.
    pub seed_paths: HashMap<ThoughtLocator, Vec<GraphExpansionPath>>,
    /// Number of power-iteration rounds executed.
    pub iterations: usize,
    /// Whether iteration stopped because score movement reached tolerance.
    pub converged: bool,
    /// Whether the reachable graph was cut off by `PprConfig::max_nodes`.
    pub truncated: bool,
}

/// Run deterministic personalized PageRank over a weighted adjacency graph.
pub fn personalized_pagerank(
    graph: &PprGraph,
    seeds: &[(ThoughtLocator, f32)],
    config: PprConfig,
) -> PprResult {
    let nodes = collect_nodes(graph, seeds, config);
    let mut scores = HashMap::new();
    if nodes.nodes.is_empty() {
        return PprResult {
            scores,
            seed_paths: HashMap::new(),
            iterations: 0,
            converged: true,
            truncated: false,
        };
    }

    let seed_distribution = seed_distribution(seeds, &nodes);
    let mut current = seed_distribution.clone();
    let damping = config.damping.clamp(0.0, 1.0);
    let tolerance = config.tolerance.max(0.0);
    let mut converged = false;
    let mut iterations = 0;

    for iteration in 0..config.max_iters {
        let mut next = teleport_vector(&seed_distribution, 1.0 - damping);
        let mut dangling_mass = 0.0;

        for node in &nodes.nodes {
            let node_score = *current.get(node).unwrap_or(&0.0);
            if node_score == 0.0 {
                continue;
            }

            let outgoing = filtered_outgoing(graph, node, &nodes, config.include_implicit_edges);
            let total_weight: f32 = outgoing.iter().map(|edge| edge.weight).sum();
            if total_weight <= 0.0 {
                dangling_mass += node_score;
                continue;
            }

            for edge in outgoing {
                *next.entry(edge.target.clone()).or_insert(0.0) +=
                    damping * node_score * (edge.weight / total_weight);
            }
        }

        if dangling_mass > 0.0 {
            for (seed, seed_weight) in &seed_distribution {
                *next.entry(seed.clone()).or_insert(0.0) += damping * dangling_mass * seed_weight;
            }
        }

        let movement = score_movement(&nodes, &current, &next);
        current = next;
        iterations = iteration + 1;
        if movement <= tolerance {
            converged = true;
            break;
        }
    }

    for node in nodes.nodes {
        if let Some(score) = current.remove(&node) {
            if score > 0.0 {
                scores.insert(node, score);
            }
        }
    }

    PprResult {
        scores,
        seed_paths: HashMap::new(),
        iterations,
        converged,
        truncated: nodes.truncated,
    }
}

#[derive(Debug, Clone)]
struct BoundedNodes {
    nodes: Vec<ThoughtLocator>,
    set: HashSet<ThoughtLocator>,
    truncated: bool,
}

fn collect_nodes(
    graph: &PprGraph,
    seeds: &[(ThoughtLocator, f32)],
    config: PprConfig,
) -> BoundedNodes {
    let max_nodes = config.max_nodes.max(1);
    let mut nodes = Vec::new();
    let mut set = HashSet::new();
    let mut queue = VecDeque::new();
    let mut truncated = false;

    for seed in sorted_positive_seeds(seeds) {
        if set.contains(&seed) {
            continue;
        }
        if nodes.len() >= max_nodes {
            truncated = true;
            break;
        }
        set.insert(seed.clone());
        nodes.push(seed.clone());
        queue.push_back(seed);
    }

    while let Some(node) = queue.pop_front() {
        for edge in sorted_edges(graph.outgoing(&node), config.include_implicit_edges) {
            if set.contains(&edge.target) {
                continue;
            }
            if nodes.len() >= max_nodes {
                truncated = true;
                break;
            }
            set.insert(edge.target.clone());
            nodes.push(edge.target.clone());
            queue.push_back(edge.target.clone());
        }
        if truncated {
            break;
        }
    }

    BoundedNodes {
        nodes,
        set,
        truncated,
    }
}

fn sorted_positive_seeds(seeds: &[(ThoughtLocator, f32)]) -> Vec<ThoughtLocator> {
    let mut weighted: Vec<(ThoughtLocator, f32)> = seeds
        .iter()
        .filter(|(_, weight)| weight.is_finite() && *weight > 0.0)
        .cloned()
        .collect();
    weighted.sort_by(|left, right| left.0.cmp(&right.0));
    weighted.into_iter().map(|(seed, _)| seed).collect()
}

fn seed_distribution(
    seeds: &[(ThoughtLocator, f32)],
    nodes: &BoundedNodes,
) -> HashMap<ThoughtLocator, f32> {
    let mut distribution = HashMap::new();
    let mut total = 0.0;
    for (seed, weight) in seeds {
        if weight.is_finite() && *weight > 0.0 && nodes.set.contains(seed) {
            *distribution.entry(seed.clone()).or_insert(0.0) += *weight;
            total += *weight;
        }
    }

    if total > 0.0 {
        for weight in distribution.values_mut() {
            *weight /= total;
        }
    }

    distribution
}

fn teleport_vector(
    seed_distribution: &HashMap<ThoughtLocator, f32>,
    weight: f32,
) -> HashMap<ThoughtLocator, f32> {
    seed_distribution
        .iter()
        .map(|(seed, seed_weight)| (seed.clone(), weight * seed_weight))
        .collect()
}

fn filtered_outgoing<'a>(
    graph: &'a PprGraph,
    node: &ThoughtLocator,
    nodes: &BoundedNodes,
    include_implicit_edges: bool,
) -> Vec<&'a PprEdge> {
    sorted_edges(graph.outgoing(node), include_implicit_edges)
        .into_iter()
        .filter(|edge| nodes.set.contains(&edge.target))
        .collect()
}

fn sorted_edges(edges: &[PprEdge], include_implicit_edges: bool) -> Vec<&PprEdge> {
    let mut edges: Vec<&PprEdge> = edges
        .iter()
        .filter(|edge| {
            edge.weight.is_finite()
                && edge.weight > 0.0
                && (include_implicit_edges || !edge.implicit)
        })
        .collect();
    edges.sort_by(|left, right| left.target.cmp(&right.target));
    edges
}

fn score_movement(
    nodes: &BoundedNodes,
    current: &HashMap<ThoughtLocator, f32>,
    next: &HashMap<ThoughtLocator, f32>,
) -> f32 {
    nodes
        .nodes
        .iter()
        .map(|node| {
            let left = current.get(node).copied().unwrap_or(0.0);
            let right = next.get(node).copied().unwrap_or(0.0);
            (left - right).abs()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn locator(index: u64) -> ThoughtLocator {
        ThoughtLocator {
            chain_key: None,
            thought_id: Uuid::from_u128(index as u128 + 1),
            thought_index: Some(index),
        }
    }

    fn assert_close(left: f32, right: f32) {
        assert!((left - right).abs() < 0.0001, "{left} != {right}");
    }

    #[test]
    fn ppr_scores_are_deterministic() {
        let a = locator(0);
        let b = locator(1);
        let c = locator(2);
        let mut graph = PprGraph::new();
        graph.add_edge(a.clone(), PprEdge::explicit(b.clone(), 2.0));
        graph.add_edge(a.clone(), PprEdge::explicit(c.clone(), 1.0));

        let config = PprConfig {
            max_iters: 50,
            tolerance: 0.0,
            ..PprConfig::default()
        };
        let first = personalized_pagerank(&graph, &[(a.clone(), 1.0)], config);
        let second = personalized_pagerank(&graph, &[(a.clone(), 1.0)], config);

        assert_eq!(first.scores, second.scores);
        assert!(first.scores[&b] > first.scores[&c]);
    }

    #[test]
    fn ppr_respects_max_nodes_and_max_iters() {
        let a = locator(0);
        let b = locator(1);
        let c = locator(2);
        let mut graph = PprGraph::new();
        graph.add_edge(a.clone(), PprEdge::explicit(b.clone(), 1.0));
        graph.add_edge(b.clone(), PprEdge::explicit(c.clone(), 1.0));

        let result = personalized_pagerank(
            &graph,
            &[(a.clone(), 1.0)],
            PprConfig {
                max_iters: 1,
                tolerance: 0.0,
                max_nodes: 2,
                ..PprConfig::default()
            },
        );

        assert_eq!(result.iterations, 1);
        assert!(result.truncated);
        assert!(result.scores.contains_key(&a));
        assert!(result.scores.contains_key(&b));
        assert!(!result.scores.contains_key(&c));
    }

    #[test]
    fn ppr_propagates_to_two_hop_node() {
        let a = locator(0);
        let b = locator(1);
        let c = locator(2);
        let mut graph = PprGraph::new();
        graph.add_edge(a.clone(), PprEdge::explicit(b.clone(), 1.0));
        graph.add_edge(b.clone(), PprEdge::explicit(c.clone(), 1.0));

        let result = personalized_pagerank(
            &graph,
            &[(a.clone(), 1.0)],
            PprConfig {
                damping: 0.5,
                max_iters: 3,
                tolerance: 0.0,
                ..PprConfig::default()
            },
        );

        assert_close(result.scores[&a], 0.625);
        assert_close(result.scores[&b], 0.25);
        assert_close(result.scores[&c], 0.125);
    }

    #[test]
    fn ppr_can_exclude_implicit_edges() {
        let a = locator(0);
        let b = locator(1);
        let c = locator(2);
        let mut graph = PprGraph::new();
        graph.add_edge(a.clone(), PprEdge::explicit(b.clone(), 1.0));
        graph.add_edge(a.clone(), PprEdge::implicit(c.clone(), 1.0));

        let result = personalized_pagerank(
            &graph,
            &[(a.clone(), 1.0)],
            PprConfig {
                include_implicit_edges: false,
                max_iters: 2,
                tolerance: 0.0,
                ..PprConfig::default()
            },
        );

        assert!(result.scores.contains_key(&b));
        assert!(!result.scores.contains_key(&c));
    }
}
