use crate::{Thought, ThoughtRelationKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

/// Direction used when traversing the thought adjacency graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AdjacencyDirection {
    /// Follow edges from the current thought to the thoughts it references or
    /// relates to.
    Outgoing,
    /// Follow reverse edges from the current thought to newer thoughts that
    /// point back to it.
    Incoming,
}

/// Stable locator for a thought in graph expansion results.
///
/// Local thoughts keep their append-order index when it is known. Cross-chain
/// targets preserve the chain key and id even when the remote chain is not
/// loaded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThoughtLocator {
    /// Optional chain key. `None` means the current chain.
    pub chain_key: Option<String>,
    /// Stable thought identifier.
    pub thought_id: Uuid,
    /// Optional append-order index for local thoughts.
    pub thought_index: Option<u64>,
}

impl ThoughtLocator {
    /// Build a locator for a local thought with a known append-order index.
    pub fn local(thought: &Thought) -> Self {
        Self {
            chain_key: None,
            thought_id: thought.id,
            thought_index: Some(thought.index),
        }
    }

    /// Build a locator for an intra-chain thought id.
    ///
    /// Use `thought_index = None` when the thought id is known but has not been
    /// resolved in the current chain snapshot.
    pub fn local_id(thought_id: Uuid, thought_index: Option<u64>) -> Self {
        Self {
            chain_key: None,
            thought_id,
            thought_index,
        }
    }

    /// Build a locator for a cross-chain thought target.
    pub fn cross_chain(chain_key: impl Into<String>, thought_id: Uuid) -> Self {
        Self {
            chain_key: Some(chain_key.into()),
            thought_id,
            thought_index: None,
        }
    }

    /// Return `true` when this locator points to the current chain.
    pub fn is_local(&self) -> bool {
        self.chain_key.is_none()
    }
}

/// Why one graph edge exists.
///
/// Multiple provenance entries may collapse onto one logical edge when the
/// same source and target are linked by both raw `refs` and richer semantic
/// relations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum GraphEdgeProvenance {
    /// The edge came from a raw `refs` entry on the source thought.
    Ref {
        /// Position inside the source thought's `refs` vector.
        ref_position: usize,
        /// Append-order index named by the ref.
        target_index: u64,
    },
    /// The edge came from a typed semantic relation on the source thought.
    Relation {
        /// Position inside the source thought's `relations` vector.
        relation_position: usize,
        /// Semantic meaning of the relation.
        kind: ThoughtRelationKind,
        /// Optional cross-chain target key carried by the original relation.
        chain_key: Option<String>,
    },
}

/// One deduplicated connection between two thought locators.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphEdge {
    /// Source thought in the local chain snapshot.
    pub source: ThoughtLocator,
    /// Target thought or remote relation endpoint.
    pub target: ThoughtLocator,
    /// Concrete reasons this edge exists.
    pub provenances: Vec<GraphEdgeProvenance>,
}

impl GraphEdge {
    /// Return the departure locator for a traversal direction.
    pub fn departure(&self, direction: AdjacencyDirection) -> &ThoughtLocator {
        match direction {
            AdjacencyDirection::Outgoing => &self.source,
            AdjacencyDirection::Incoming => &self.target,
        }
    }

    /// Return the arrival locator for a traversal direction.
    pub fn arrival(&self, direction: AdjacencyDirection) -> &ThoughtLocator {
        match direction {
            AdjacencyDirection::Outgoing => &self.target,
            AdjacencyDirection::Incoming => &self.source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EdgeKey {
    source: ThoughtLocator,
    target: ThoughtLocator,
}

/// Read-only adjacency built from a slice of committed thoughts.
///
/// The index preserves outgoing and incoming edges so that later search layers
/// can expand lexical seeds in either direction without re-scanning the whole
/// chain.
#[derive(Debug, Clone, Default)]
pub struct ThoughtAdjacencyIndex {
    local_by_id: HashMap<Uuid, ThoughtLocator>,
    local_by_index: BTreeMap<u64, ThoughtLocator>,
    outgoing: BTreeMap<ThoughtLocator, Vec<GraphEdge>>,
    incoming: BTreeMap<ThoughtLocator, Vec<GraphEdge>>,
}

impl ThoughtAdjacencyIndex {
    /// Build a new adjacency snapshot from committed thoughts.
    pub fn from_thoughts(thoughts: &[Thought]) -> Self {
        let mut local_by_id = HashMap::new();
        let mut local_by_index = BTreeMap::new();
        let mut outgoing: BTreeMap<ThoughtLocator, Vec<GraphEdge>> = BTreeMap::new();
        let mut incoming: BTreeMap<ThoughtLocator, Vec<GraphEdge>> = BTreeMap::new();

        for thought in thoughts {
            let locator = ThoughtLocator::local(thought);
            local_by_id.insert(thought.id, locator.clone());
            local_by_index.insert(thought.index, locator.clone());
            outgoing.entry(locator.clone()).or_default();
            incoming.entry(locator).or_default();
        }

        let mut edge_provenance: BTreeMap<EdgeKey, Vec<GraphEdgeProvenance>> = BTreeMap::new();
        for thought in thoughts {
            let source = local_by_id
                .get(&thought.id)
                .cloned()
                .unwrap_or_else(|| ThoughtLocator::local(thought));

            for (ref_position, target_index) in thought.refs.iter().copied().enumerate() {
                if let Some(target) = local_by_index.get(&target_index).cloned() {
                    edge_provenance
                        .entry(EdgeKey {
                            source: source.clone(),
                            target,
                        })
                        .or_default()
                        .push(GraphEdgeProvenance::Ref {
                            ref_position,
                            target_index,
                        });
                }
            }

            for (relation_position, relation) in thought.relations.iter().enumerate() {
                let target = match relation.chain_key.as_deref() {
                    Some(chain_key) => {
                        ThoughtLocator::cross_chain(chain_key.to_string(), relation.target_id)
                    }
                    None => local_by_id
                        .get(&relation.target_id)
                        .cloned()
                        .unwrap_or_else(|| ThoughtLocator::local_id(relation.target_id, None)),
                };
                edge_provenance
                    .entry(EdgeKey {
                        source: source.clone(),
                        target,
                    })
                    .or_default()
                    .push(GraphEdgeProvenance::Relation {
                        relation_position,
                        kind: relation.kind,
                        chain_key: relation.chain_key.clone(),
                    });
            }
        }

        for (key, mut provenances) in edge_provenance {
            dedupe_and_sort_provenances(&mut provenances);
            let edge = GraphEdge {
                source: key.source.clone(),
                target: key.target.clone(),
                provenances,
            };
            outgoing
                .entry(edge.source.clone())
                .or_default()
                .push(edge.clone());
            incoming.entry(edge.target.clone()).or_default().push(edge);
        }

        for edges in outgoing.values_mut() {
            edges.sort_by(|left, right| left.target.cmp(&right.target));
        }
        for edges in incoming.values_mut() {
            edges.sort_by(|left, right| left.source.cmp(&right.source));
        }

        Self {
            local_by_id,
            local_by_index,
            outgoing,
            incoming,
        }
    }

    /// Return the local locator for a thought id in this snapshot.
    pub fn local_locator_for_id(&self, thought_id: Uuid) -> Option<&ThoughtLocator> {
        self.local_by_id.get(&thought_id)
    }

    /// Return a reference to the underlying id→locator map.
    pub fn locator_map(&self) -> &HashMap<Uuid, ThoughtLocator> {
        &self.local_by_id
    }

    /// Return the local locator for an append-order index in this snapshot.
    pub fn local_locator_for_index(&self, thought_index: u64) -> Option<&ThoughtLocator> {
        self.local_by_index.get(&thought_index)
    }

    /// Return outgoing edges from `node`.
    pub fn outgoing(&self, node: &ThoughtLocator) -> &[GraphEdge] {
        self.outgoing.get(node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Return incoming edges into `node`.
    pub fn incoming(&self, node: &ThoughtLocator) -> &[GraphEdge] {
        self.incoming.get(node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Return neighbors for one traversal direction.
    pub fn neighbors(&self, node: &ThoughtLocator, direction: AdjacencyDirection) -> &[GraphEdge] {
        match direction {
            AdjacencyDirection::Outgoing => self.outgoing(node),
            AdjacencyDirection::Incoming => self.incoming(node),
        }
    }
}

fn dedupe_and_sort_provenances(provenances: &mut Vec<GraphEdgeProvenance>) {
    let mut seen = HashSet::new();
    provenances.retain(|provenance| seen.insert(provenance.clone()));
    provenances.sort_by_key(provenance_sort_key);
}

fn provenance_sort_key(provenance: &GraphEdgeProvenance) -> (u8, usize, String, Option<String>) {
    match provenance {
        GraphEdgeProvenance::Ref {
            ref_position,
            target_index,
        } => (0, *ref_position, target_index.to_string(), None),
        GraphEdgeProvenance::Relation {
            relation_position,
            kind,
            chain_key,
        } => (
            1,
            *relation_position,
            format!("{kind:?}"),
            chain_key.clone(),
        ),
    }
}
