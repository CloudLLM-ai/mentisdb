//! Retrieval must hide superseded / corrected / invalidated thoughts by default.
//!
//! Covers `query`, `query_ranked`, `query_context_bundles`, and point-in-time
//! (`as_of`) semantics so agents see current memory without opting into audit
//! mode via `include_invalidated`.

use chrono::Utc;
use mentisdb::{
    MentisDb, RankedSearchGraph, RankedSearchQuery, StorageAdapterKind, ThoughtInput,
    ThoughtQuery, ThoughtRelation, ThoughtRelationKind, ThoughtType,
};
use std::path::PathBuf;
use tempfile::tempdir;
use uuid::Uuid;

fn open_chain(dir: &std::path::Path, key: &str) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, key, StorageAdapterKind::Binary).unwrap()
}

fn append(
    chain: &mut MentisDb,
    thought_type: ThoughtType,
    content: &str,
    relations: Vec<ThoughtRelation>,
) -> mentisdb::Thought {
    let mut input = ThoughtInput::new(thought_type, content).with_importance(0.9);
    if !relations.is_empty() {
        input = input.with_relations(relations);
    }
    chain.append_thought("agent", input).unwrap().clone()
}

/// Superseded thoughts disappear from default query and ranked search, but
/// remain available when include_invalidated is set.
#[test]
fn default_search_excludes_superseded_thoughts() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "inv");

    let original = append(
        &mut chain,
        ThoughtType::Decision,
        "We use Redis for session cache",
        vec![],
    );
    let _replacement = append(
        &mut chain,
        ThoughtType::Decision,
        "We use Postgres for session cache",
        vec![ThoughtRelation {
            kind: ThoughtRelationKind::Supersedes,
            target_id: original.id,
            chain_key: None,
            valid_at: Some(Utc::now()),
            invalid_at: None,
        }],
    );

    assert!(chain.is_invalidated(original.id));
    assert_eq!(chain.invalidated_thought_ids().len(), 1);

    let default_hits = chain.query(&ThoughtQuery::new().with_text("session cache"));
    assert_eq!(default_hits.len(), 1);
    assert_eq!(default_hits[0].content, "We use Postgres for session cache");

    let audit_hits = chain.query(
        &ThoughtQuery::new()
            .with_text("session cache")
            .with_include_invalidated(true),
    );
    assert_eq!(audit_hits.len(), 2);

    let ranked = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("session cache")
            .with_limit(10),
    );
    assert_eq!(ranked.hits.len(), 1);
    assert_eq!(
        ranked.hits[0].thought.content,
        "We use Postgres for session cache"
    );

    let ranked_audit = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("session cache")
            .with_include_invalidated(true)
            .with_limit(10),
    );
    assert_eq!(ranked_audit.hits.len(), 2);
}

/// Corrects and Invalidates edges also feed the invalidation set.
#[test]
fn corrects_and_invalidates_exclude_targets_from_ranked_search() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "inv2");

    let wrong = append(
        &mut chain,
        ThoughtType::FactLearned,
        "API rate limit is 100 req/s",
        vec![],
    );
    let assumption = append(
        &mut chain,
        ThoughtType::Hypothesis,
        "The bug is in the client SDK",
        vec![],
    );
    let _ = append(
        &mut chain,
        ThoughtType::Correction,
        "API rate limit is 1000 req/s",
        vec![ThoughtRelation {
            kind: ThoughtRelationKind::Corrects,
            target_id: wrong.id,
            chain_key: None,
            valid_at: Some(Utc::now()),
            invalid_at: None,
        }],
    );
    let _ = append(
        &mut chain,
        ThoughtType::AssumptionInvalidated,
        "The bug was in our server middleware, not the client",
        vec![ThoughtRelation {
            kind: ThoughtRelationKind::Invalidates,
            target_id: assumption.id,
            chain_key: None,
            valid_at: Some(Utc::now()),
            invalid_at: None,
        }],
    );

    assert!(chain.is_invalidated(wrong.id));
    assert!(chain.is_invalidated(assumption.id));

    let rate = chain.query_ranked(&RankedSearchQuery::new().with_text("rate limit").with_limit(5));
    assert!(rate
        .hits
        .iter()
        .all(|h| h.thought.id != wrong.id));
    assert!(rate
        .hits
        .iter()
        .any(|h| h.thought.content.contains("1000")));

    let bug = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("bug client SDK")
            .with_limit(5),
    );
    assert!(bug.hits.iter().all(|h| h.thought.id != assumption.id));
}

/// Point-in-time search still surfaces a thought that was valid at as_of,
/// even if it was superseded later.
#[test]
fn as_of_keeps_thoughts_valid_at_that_time() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "asof");

    let original = append(
        &mut chain,
        ThoughtType::Decision,
        "Deploy to us-east-1 only",
        vec![],
    );
    // Point-in-time at the original commit: supersede has not happened yet.
    let as_of_before_supersede = original.timestamp;
    let _ = append(
        &mut chain,
        ThoughtType::Decision,
        "Deploy multi-region",
        vec![ThoughtRelation {
            kind: ThoughtRelationKind::Supersedes,
            target_id: original.id,
            chain_key: None,
            valid_at: Some(Utc::now()),
            invalid_at: None,
        }],
    );

    assert!(chain.is_invalidated(original.id));
    assert!(!chain.is_invalidated_as_of(original.id, as_of_before_supersede));
    assert!(chain.is_invalidated_as_of(original.id, Utc::now()));

    let historical = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("Deploy")
            .with_as_of(as_of_before_supersede)
            .with_limit(10),
    );
    assert!(
        historical.hits.iter().any(|h| h.thought.id == original.id),
        "historical query should still see the pre-supersede decision"
    );

    let live = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("Deploy")
            .with_limit(10),
    );
    assert!(live.hits.iter().all(|h| h.thought.id != original.id));
}

/// Context bundles seed selection also drops invalidated thoughts.
#[test]
fn context_bundles_exclude_invalidated_seeds() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "bundles");

    let old = append(
        &mut chain,
        ThoughtType::Insight,
        "caching layer uses LRU only",
        vec![],
    );
    let _ = append(
        &mut chain,
        ThoughtType::Insight,
        "caching layer uses LRU plus TTL eviction",
        vec![ThoughtRelation {
            kind: ThoughtRelationKind::Supersedes,
            target_id: old.id,
            chain_key: None,
            valid_at: Some(Utc::now()),
            invalid_at: None,
        }],
    );

    let bundles = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_text("caching layer LRU")
            .with_graph(RankedSearchGraph::default())
            .with_limit(5),
    );
    assert!(
        !bundles.bundles.is_empty(),
        "expected at least one bundle from live insight"
    );
    for bundle in &bundles.bundles {
        if let Some(idx) = bundle.seed.locator.thought_index {
            let thought = &chain.thoughts()[idx as usize];
            assert_ne!(thought.id, old.id);
            assert!(!chain.is_invalidated(thought.id));
        }
    }
}

/// Invalidation index rebuilds correctly when reopening a durable chain.
#[test]
fn invalidated_set_rebuilds_on_reopen() {
    let dir = tempdir().unwrap();
    let path = PathBuf::from(dir.path());
    let original_id = {
        let mut chain = open_chain(&path, "persist");
        let original = append(
            &mut chain,
            ThoughtType::LessonLearned,
            "Always pin dependency versions",
            vec![],
        );
        let _ = append(
            &mut chain,
            ThoughtType::LessonLearned,
            "Pin dependency versions except for security patches",
            vec![ThoughtRelation {
                kind: ThoughtRelationKind::Supersedes,
                target_id: original.id,
                chain_key: None,
                valid_at: Some(Utc::now()),
                invalid_at: None,
            }],
        );
        original.id
    };

    let reopened = open_chain(&path, "persist");
    assert!(reopened.is_invalidated(original_id));
    let hits = reopened.query(&ThoughtQuery::new().with_text("dependency versions"));
    assert_eq!(hits.len(), 1);
    assert!(!hits.iter().any(|t| t.id == original_id));
}

#[test]
fn unknown_id_is_not_invalidated() {
    let dir = tempdir().unwrap();
    let chain = open_chain(dir.path(), "empty-ish");
    assert!(!chain.is_invalidated(Uuid::new_v4()));
}
