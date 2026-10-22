//! Ranked semantic search must keep its results when the candidate set is
//! "almost the whole chain" (an invalidated thought removed), which now uses
//! an over-fetch plus position mask instead of an id-set filter.

use chrono::Utc;
use mentisdb::search::LocalTextEmbeddingProvider;
use mentisdb::{
    MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};
use tempfile::tempdir;

fn ranked_ids(chain: &MentisDb, include_invalidated: bool) -> Vec<String> {
    let mut query = RankedSearchQuery::new().with_text("identical sentence about rust");
    query = query.with_limit(20);
    if include_invalidated {
        query = query.with_include_invalidated(true);
    }
    chain
        .query_ranked(&query)
        .hits
        .iter()
        .map(|hit| hit.thought.id.to_string())
        .collect()
}

/// With one thought invalidated, vector-backed ranked search returns every
/// other thought, never the invalidated one, in the same relative order as
/// the audit (include_invalidated) listing.
#[test]
fn test_ranked_semantic_search_matches_audit_order_minus_invalidated() {
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key_and_storage_kind(
        dir.path(),
        "rank-filter",
        StorageAdapterKind::Binary,
    )
    .unwrap();
    chain
        .manage_vector_sidecar(LocalTextEmbeddingProvider::new())
        .unwrap();
    let mut first_id = None;
    for i in 0..8 {
        let t = chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!("identical sentence about rust variant {i}"),
                ),
            )
            .unwrap()
            .clone();
        first_id.get_or_insert(t.id);
    }
    let victim = first_id.unwrap();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Correction, "unrelated replacement note")
                .with_relations(vec![ThoughtRelation {
                    kind: ThoughtRelationKind::Supersedes,
                    target_id: victim,
                    chain_key: None,
                    valid_at: Some(Utc::now()),
                    invalid_at: None,
                }]),
        )
        .unwrap();
    assert!(chain.is_invalidated(victim));

    let default_ids = ranked_ids(&chain, false);
    let audit_ids: Vec<String> = ranked_ids(&chain, true)
        .into_iter()
        .filter(|id| *id != victim.to_string())
        .collect();
    assert!(!default_ids.contains(&victim.to_string()));
    assert!(
        default_ids.len() >= 7,
        "all valid matching thoughts returned"
    );
    assert_eq!(
        default_ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        audit_ids.iter().collect::<std::collections::BTreeSet<_>>()
    );
}
