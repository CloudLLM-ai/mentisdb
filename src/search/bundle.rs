use super::{GraphEdgeProvenance, GraphExpansionHit, GraphExpansionPath, ThoughtLocator};
use crate::ThoughtRelationKind;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One lexical seed used to anchor a context bundle.
///
/// Callers typically derive these from ranked lexical hits and preserve their
/// desired presentation order before building bundles.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBundleSeed {
    /// Stable thought locator of the lexical seed.
    pub locator: ThoughtLocator,
    /// Lexical score assigned to this seed by the retrieval layer.
    pub lexical_score: f32,
    /// Normalized lexical terms that matched the seed.
    pub matched_terms: Vec<String>,
}

impl ContextBundleSeed {
    /// Create a seed with locator and lexical score.
    pub fn new(locator: ThoughtLocator, lexical_score: f32) -> Self {
        Self {
            locator,
            lexical_score,
            matched_terms: Vec::new(),
        }
    }

    /// Set the matched lexical terms for this seed.
    pub fn with_matched_terms<I, S>(mut self, matched_terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.matched_terms = matched_terms.into_iter().map(Into::into).collect();
        self
    }
}

/// One supporting thought grouped under a bundle seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBundleHit {
    /// Supporting thought reached from the seed.
    pub locator: ThoughtLocator,
    /// Shortest discovered depth from the seed.
    pub depth: usize,
    /// Best provenance path from the seed to the supporting thought.
    pub path: GraphExpansionPath,
    /// Distinct relation kinds encountered along the chosen path.
    pub relation_kinds: Vec<ThoughtRelationKind>,
    /// Number of candidate paths from this seed to this locator seen in input.
    pub seed_path_count: usize,
}

/// Grouped supporting context anchored on one lexical seed.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBundle {
    /// Lexical seed thought for this bundle.
    pub seed: ContextBundleSeed,
    /// Deterministically ordered supporting thoughts for this seed.
    pub support: Vec<ContextBundleHit>,
}

/// Output of deterministic context-bundle rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBundleResult {
    /// Bundles rendered in seed order.
    pub bundles: Vec<ContextBundle>,
    /// Number of raw graph hits consumed while rendering.
    pub consumed_hits: usize,
}

/// Rendering options for context-bundle construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ContextBundleOptions {
    /// Keep depth-0 seed hits in each bundle's `support` list.
    pub include_seed_hits: bool,
}

impl ContextBundleOptions {
    /// Create default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Control whether depth-0 seed nodes are included in support hits.
    pub fn with_include_seed_hits(mut self, include_seed_hits: bool) -> Self {
        self.include_seed_hits = include_seed_hits;
        self
    }
}

/// Build deterministic seed-anchored bundles from graph-expansion hits.
///
/// `seeds` controls bundle ordering. `graph_hits` may contain mixed-seed hits;
/// only hits whose `path.seed` appears in `seeds` are included.
pub fn build_context_bundles(
    seeds: &[ContextBundleSeed],
    graph_hits: &[GraphExpansionHit],
    options: ContextBundleOptions,
) -> ContextBundleResult {
    let mut ordered_seeds = Vec::with_capacity(seeds.len());
    let mut seen_seed_locators = HashSet::new();
    for seed in seeds {
        if seen_seed_locators.insert(seed.locator.clone()) {
            ordered_seeds.push(seed.clone());
        }
    }

    let seed_positions: HashMap<ThoughtLocator, usize> = ordered_seeds
        .iter()
        .enumerate()
        .map(|(index, seed)| (seed.locator.clone(), index))
        .collect();
    let mut grouped: Vec<BTreeMap<ThoughtLocator, BundleAccumulator>> =
        vec![BTreeMap::new(); ordered_seeds.len()];
    let mut consumed_hits = 0_usize;

    for hit in graph_hits {
        let Some(&bundle_index) = seed_positions.get(&hit.path.seed) else {
            continue;
        };
        if !options.include_seed_hits && hit.depth == 0 {
            continue;
        }
        consumed_hits += 1;

        grouped[bundle_index]
            .entry(hit.locator.clone())
            .and_modify(|entry| entry.observe(hit))
            .or_insert_with(|| BundleAccumulator::new(hit));
    }

    let bundles = ordered_seeds
        .into_iter()
        .zip(grouped)
        .map(|(seed, entries)| {
            let mut support: Vec<ContextBundleHit> = entries
                .into_values()
                .map(BundleAccumulator::into_hit)
                .collect();
            support.sort_by(|left, right| {
                left.depth.cmp(&right.depth).then_with(|| {
                    bundle_locator_order_key(&left.locator)
                        .cmp(&bundle_locator_order_key(&right.locator))
                })
            });
            ContextBundle { seed, support }
        })
        .collect();

    ContextBundleResult {
        bundles,
        consumed_hits,
    }
}

#[derive(Debug, Clone)]
struct BundleAccumulator {
    depth: usize,
    path: GraphExpansionPath,
    relation_kinds: Vec<ThoughtRelationKind>,
    seed_path_count: usize,
}

impl BundleAccumulator {
    fn new(hit: &GraphExpansionHit) -> Self {
        Self {
            depth: hit.depth,
            path: hit.path.clone(),
            relation_kinds: relation_kinds_from_path(&hit.path),
            seed_path_count: 1,
        }
    }

    fn observe(&mut self, hit: &GraphExpansionHit) {
        self.seed_path_count += 1;
        if hit.depth < self.depth
            || (hit.depth == self.depth && path_order_key(&hit.path) < path_order_key(&self.path))
        {
            self.depth = hit.depth;
            self.path = hit.path.clone();
            self.relation_kinds = relation_kinds_from_path(&hit.path);
        }
    }

    fn into_hit(self) -> ContextBundleHit {
        ContextBundleHit {
            locator: self.path.current().clone(),
            depth: self.depth,
            path: self.path,
            relation_kinds: self.relation_kinds,
            seed_path_count: self.seed_path_count,
        }
    }
}

fn relation_kinds_from_path(path: &GraphExpansionPath) -> Vec<ThoughtRelationKind> {
    let mut kinds = Vec::new();
    for hop in &path.hops {
        for provenance in &hop.edge.provenances {
            if let GraphEdgeProvenance::Relation { kind, .. } = provenance {
                if !kinds.contains(kind) {
                    kinds.push(*kind);
                }
            }
        }
    }
    kinds
}

fn path_order_key(path: &GraphExpansionPath) -> Vec<ThoughtLocator> {
    path.visited().into_iter().cloned().collect()
}

fn bundle_locator_order_key(locator: &ThoughtLocator) -> (Option<&str>, Option<u64>, uuid::Uuid) {
    (
        locator.chain_key.as_deref(),
        locator.thought_index,
        locator.thought_id,
    )
}
