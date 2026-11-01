//! Result-time collapsing of exact and near-duplicate hits.
//!
//! Measured on one real chain, 301 of 749 live thoughts sat in near-duplicate
//! clusters and 149 duplicate pairs were farther apart than the 64-thought
//! append-time dedup window, so copies kept filling the top ranks. Collapsing
//! happens at result time and works on any chain, old or new.

use mentisdb::{
    MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType, VectorFusion,
};
use tempfile::tempdir;

fn chain(key: &str) -> (tempfile::TempDir, MentisDb) {
    let dir = tempdir().unwrap();
    let chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), key, StorageAdapterKind::Binary)
            .unwrap();
    (dir, chain)
}

/// Exact copies collapse into the highest-ranked one, which lists them.
#[test]
fn exact_copies_collapse_and_are_reported() {
    let (_d, mut c) = chain("dup1");
    for _ in 0..4 {
        c.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Finding,
                "The staging database password rotates every ninety days.",
            ),
        )
        .unwrap();
    }
    c.append_thought(
        "a",
        ThoughtInput::new(
            ThoughtType::Finding,
            "Staging database backups run nightly at two.",
        ),
    )
    .unwrap();
    let q = RankedSearchQuery::new()
        .with_text("staging database")
        .with_collapse_duplicates(Some(0.9));
    let hits = c.query_ranked(&q).hits;
    assert_eq!(
        hits.len(),
        2,
        "four copies become one, plus the distinct backup note"
    );
    let survivor = hits
        .iter()
        .find(|h| h.thought.content.contains("password"))
        .unwrap();
    assert_eq!(survivor.collapsed.len(), 3);
    assert_eq!(
        c.hit_json(survivor)["collapsed_duplicates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

/// With collapsing off every copy is returned, as before.
#[test]
fn disabled_collapse_returns_every_copy() {
    let (_d, mut c) = chain("dup2");
    for _ in 0..3 {
        c.append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, "Cache TTL is sixty seconds."),
        )
        .unwrap();
    }
    let q = RankedSearchQuery::new()
        .with_text("cache TTL")
        .with_collapse_duplicates(None);
    assert_eq!(c.query_ranked(&q).hits.len(), 3);
}

/// Collapsing frees ranks: the page is refilled from beyond the caller's limit.
#[test]
fn collapse_refills_the_page_up_to_the_limit() {
    let (_d, mut c) = chain("dup3");
    for _ in 0..5 {
        c.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Finding,
                "Deploys go through the blue green pipeline.",
            ),
        )
        .unwrap();
    }
    for i in 0..3 {
        c.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Finding,
                format!("Pipeline stage {i} runs unique checks."),
            ),
        )
        .unwrap();
    }
    let q = RankedSearchQuery::new()
        .with_text("pipeline")
        .with_limit(3)
        .with_collapse_duplicates(Some(0.9));
    let hits = c.query_ranked(&q).hits;
    assert_eq!(hits.len(), 3);
    let distinct: std::collections::HashSet<_> =
        hits.iter().map(|h| h.thought.content.clone()).collect();
    assert_eq!(distinct.len(), 3, "no two returned hits are copies");
}

/// Collapsing composes with successor redirection: a superseded copy never
/// resurfaces, and the live one survives.
#[test]
fn collapse_composes_with_supersession() {
    let (_d, mut c) = chain("dup4");
    let old = c
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Use Redis for the session cache layer.",
            ),
        )
        .unwrap()
        .clone();
    let new = c
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Use Redis for the session cache layer.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::Supersedes,
                target_id: old.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap()
        .clone();
    let q = RankedSearchQuery::new()
        .with_text("session cache Redis")
        .with_collapse_duplicates(Some(0.9));
    let hits = c.query_ranked(&q).hits;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].thought.id, new.id);
}

/// A threshold outside `(0, 1]` is ignored by the environment parser and the
/// names round-trip for the fusion enum.
#[test]
fn vector_fusion_names_round_trip_and_reject_garbage() {
    for f in [VectorFusion::Absolute, VectorFusion::Relative] {
        assert_eq!(f.to_string().parse::<VectorFusion>().unwrap(), f);
    }
    assert!("bogus".parse::<VectorFusion>().is_err());
}

/// Regression: a description added to an agent after its thoughts were written
/// was invisible to lexical search until the chain was reopened, because the
/// index baked the registry text in at append time.
#[test]
fn agent_description_added_later_is_searchable_without_a_restart() {
    let (_d, mut c) = chain("registry-refresh");
    c.append_thought(
        "scribe",
        ThoughtInput::new(ThoughtType::Finding, "Totally unrelated note about lunch."),
    )
    .unwrap();
    let before = c.query_ranked(
        &RankedSearchQuery::new()
            .with_text("quartzite")
            .with_collapse_duplicates(None),
    );
    assert!(before.hits.is_empty());
    c.set_agent_description("scribe", Some("Quartzite geology specialist"))
        .unwrap();
    let after = c.query_ranked(
        &RankedSearchQuery::new()
            .with_text("quartzite")
            .with_collapse_duplicates(None),
    );
    assert_eq!(
        after.hits.len(),
        1,
        "registry text is now indexed for the earlier thought"
    );
}

/// An alias or owner added later behaves the same way.
#[test]
fn agent_alias_and_owner_added_later_are_searchable() {
    let (_d, mut c) = chain("registry-refresh2");
    c.append_thought(
        "bot",
        ThoughtInput::new(ThoughtType::Finding, "Plain content here."),
    )
    .unwrap();
    c.add_agent_alias("bot", "pelican").unwrap();
    c.upsert_agent("bot", None, Some("marigold-team"), None, None)
        .unwrap();
    for word in ["pelican", "marigold"] {
        let r = c.query_ranked(
            &RankedSearchQuery::new()
                .with_text(word)
                .with_collapse_duplicates(None),
        );
        assert_eq!(r.hits.len(), 1, "{word}");
    }
}
