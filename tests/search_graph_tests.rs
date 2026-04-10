use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::{
    AdjacencyDirection, GraphEdgeProvenance, GraphExpansionPath, GraphExpansionPathError,
    ThoughtAdjacencyIndex, ThoughtLocator,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType};
use uuid::Uuid;

static SEARCH_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = SEARCH_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_graph_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn adjacency_merges_ref_and_relation_provenance_for_one_target() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-graph-provenance").unwrap();

    let base = chain
        .append(
            "astro",
            ThoughtType::FactLearned,
            "The lexical pass should seed graph expansion.",
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Search summary keeps the seed link.")
                .with_refs(vec![0])
                .with_relations(vec![ThoughtRelation {
                    kind: ThoughtRelationKind::Supports,
                    target_id: base.id,
                    chain_key: None,
                    valid_at: None,
                    invalid_at: None,
                }]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let summary = adjacency.local_locator_for_index(1).unwrap();
    let base_locator = adjacency.local_locator_for_index(0).unwrap();
    let edges = adjacency.outgoing(summary);

    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source, *summary);
    assert_eq!(edges[0].target, *base_locator);
    assert_eq!(
        edges[0].provenances,
        vec![
            GraphEdgeProvenance::Ref {
                ref_position: 0,
                target_index: 0,
            },
            GraphEdgeProvenance::Relation {
                relation_position: 0,
                kind: ThoughtRelationKind::Supports,
                chain_key: None,
            },
            GraphEdgeProvenance::Relation {
                relation_position: 1,
                kind: ThoughtRelationKind::References,
                chain_key: None,
            },
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adjacency_tracks_incoming_edges_for_local_and_cross_chain_targets() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-graph-incoming").unwrap();

    let anchor = chain
        .append(
            "astro",
            ThoughtType::Decision,
            "Rank lexical seeds before graph expansion.",
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Hypothesis,
                "Backlinks should be expandable as incoming context.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: anchor.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();

    let remote_id = Uuid::new_v4();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Plan,
                "Federated search can later follow cross-chain relations.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::ContinuesFrom,
                target_id: remote_id,
                chain_key: Some("remote-brain".to_string()),
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let anchor_locator = adjacency.local_locator_for_id(anchor.id).unwrap();
    let hypothesis_locator = adjacency.local_locator_for_index(1).unwrap();
    let plan_locator = adjacency.local_locator_for_index(2).unwrap();
    let remote_locator = ThoughtLocator::cross_chain("remote-brain", remote_id);

    let incoming_anchor = adjacency.incoming(anchor_locator);
    assert_eq!(incoming_anchor.len(), 1);
    assert_eq!(incoming_anchor[0].source, *hypothesis_locator);
    assert_eq!(incoming_anchor[0].target, *anchor_locator);

    let remote_incoming = adjacency.incoming(&remote_locator);
    assert_eq!(remote_incoming.len(), 1);
    assert_eq!(remote_incoming[0].source, *plan_locator);
    assert_eq!(remote_incoming[0].target, remote_locator);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expansion_path_supports_incoming_walks_and_rejects_cycles() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "search-graph-path").unwrap();

    let seed = chain
        .append(
            "astro",
            ThoughtType::Finding,
            "A lexical seed should expose both parents and children.",
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Insight,
                "An incoming hop should reach newer follow-up thoughts.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();

    let adjacency = ThoughtAdjacencyIndex::from_thoughts(chain.thoughts());
    let seed_locator = adjacency.local_locator_for_index(0).unwrap().clone();
    let follower_locator = adjacency.local_locator_for_index(1).unwrap().clone();
    let incoming_edge = adjacency.incoming(&seed_locator)[0].clone();

    let path = GraphExpansionPath::new(seed_locator.clone())
        .extend(AdjacencyDirection::Incoming, &incoming_edge)
        .unwrap();
    assert_eq!(path.current(), &follower_locator);
    assert_eq!(path.depth(), 1);
    assert_eq!(path.visited(), vec![&seed_locator, &follower_locator]);

    let outgoing_edge = adjacency.outgoing(&follower_locator)[0].clone();
    let error = path
        .extend(AdjacencyDirection::Outgoing, &outgoing_edge)
        .unwrap_err();
    assert_eq!(
        error,
        GraphExpansionPathError::Cycle {
            locator: seed_locator,
        }
    );

    let _ = std::fs::remove_dir_all(&dir);
}
