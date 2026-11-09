//! Regression tests for superseded versions starving live search results.
//!
//! One decision rewritten thirty times used to fill the whole "wide" ranking
//! window (`2 * limit`) that lineage resolution collapsed afterwards, so a
//! `limit = 5` search returned a single hit (the newest version) and every
//! other matching live memory was missing even though `total` counted them.
//! These tests pin the fixed behavior: only live memories compete for the
//! page, and a superseded version only nominates its live successor.

use mentisdb::{
    MentisDb, RankedSearchQuery, StorageAdapterKind, Thought, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType, VersionStatus,
};
use std::collections::HashSet;
use tempfile::TempDir;
use uuid::Uuid;

const QUERY: &str = "nightly build server status";
const VERSIONS: usize = 30;
const LIVE_FACTS: usize = 20;

fn rel(kind: ThoughtRelationKind, target: &Thought) -> ThoughtRelation {
    ThoughtRelation {
        kind,
        target_id: target.id,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }
}

/// A chain with one decision rewritten [`VERSIONS`] times (each version
/// supersedes the previous) and [`LIVE_FACTS`] separate live facts that also
/// match [`QUERY`]. Returns the chain and the ids of every version, oldest
/// first.
fn starved_chain() -> (TempDir, MentisDb, Vec<Uuid>) {
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "sup", StorageAdapterKind::Binary)
            .unwrap();
    add_filler(&mut chain);
    let mut ids: Vec<Uuid> = Vec::new();
    let mut previous: Option<Thought> = None;
    for v in 1..=VERSIONS {
        let mut input = ThoughtInput::new(
            ThoughtType::Decision,
            format!("Nightly build server status policy revision {v}: keep the runner warm."),
        );
        if let Some(prev) = &previous {
            input = input.with_relations(vec![rel(ThoughtRelationKind::Supersedes, prev)]);
        }
        let t = chain.append_thought("agent", input).unwrap().clone();
        ids.push(t.id);
        previous = Some(t);
    }
    for i in 0..LIVE_FACTS {
        chain
            .append_thought(
                "agent",
                ThoughtInput::new(
                    ThoughtType::Insight,
                    format!("Separate fact {i}: the nightly build server disk is volume {i}."),
                ),
            )
            .unwrap();
    }
    (dir, chain, ids)
}

/// Append unrelated memories so the query terms stay under the lexical
/// document-frequency cutoff (terms in more than 30% of the thoughts are
/// ignored, which would make every small test chain return nothing).
fn add_filler(chain: &mut MentisDb) {
    for i in 0..300 {
        chain
            .append_thought(
                "filler",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!("Gardening journal entry {i}: tomatoes ripen."),
                ),
            )
            .unwrap();
    }
}

fn distinct(ids: &[Uuid]) -> usize {
    ids.iter().collect::<HashSet<_>>().len()
}

/// Ranked search returns `limit` distinct live hits with the newest version
/// first, for several page sizes, and never a superseded version.
#[test]
fn test_ranked_page_is_filled_with_distinct_live_memories() {
    let (_dir, chain, ids) = starved_chain();
    for limit in [1usize, 5, 10, LIVE_FACTS + 1, 50] {
        let result =
            chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY).with_limit(limit));
        let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
        assert_eq!(hit_ids.len(), limit.min(LIVE_FACTS + 1), "limit {limit}");
        assert_eq!(distinct(&hit_ids), hit_ids.len(), "limit {limit}: distinct");
        assert_eq!(hit_ids[0], ids[VERSIONS - 1], "limit {limit}: v30 leads");
        assert!(
            hit_ids.iter().all(|id| !ids[..VERSIONS - 1].contains(id)),
            "limit {limit}: no superseded version"
        );
        assert_eq!(
            result.total_candidates,
            LIVE_FACTS + 1,
            "limit {limit}: total counts live"
        );
    }
}

/// The lineage head reports the superseded versions that nominated it.
#[test]
fn test_head_lists_the_versions_it_stands_in_for() {
    let (_dir, chain, ids) = starved_chain();
    let result = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY).with_limit(5));
    let head = &result.hits[0];
    assert!(!head.version.successor_of.is_empty());
    assert!(head
        .version
        .successor_of
        .iter()
        .all(|id| ids[..VERSIONS - 1].contains(id)));
}

/// `include_invalidated` is the audit search: old versions compete too.
#[test]
fn test_include_invalidated_returns_old_versions() {
    let (_dir, chain, ids) = starved_chain();
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_limit(10)
            .with_include_invalidated(true),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(hit_ids.len(), 10);
    assert!(hit_ids.iter().any(|id| ids[..VERSIONS - 1].contains(id)));
    assert_eq!(result.total_candidates, VERSIONS + LIVE_FACTS);
}

/// A successor that shares no word with the query still takes the place of
/// its superseded predecessor, and the live matches still fill the page.
#[test]
fn test_successor_with_unmatched_wording_still_surfaces() {
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "para", StorageAdapterKind::Binary)
            .unwrap();
    add_filler(&mut chain);
    let mut previous = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, "Release policy: deploy on Fridays."),
        )
        .unwrap()
        .clone();
    for _ in 0..12 {
        previous = chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Decision,
                    "Release policy: deploy on Fridays again.",
                )
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &previous)]),
            )
            .unwrap()
            .clone();
    }
    let paraphrase = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Decision, "Ship only early in the week.")
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &previous)]),
        )
        .unwrap()
        .clone();
    for i in 0..8 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Insight, format!("Release policy note {i}.")),
            )
            .unwrap();
    }
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text("release policy Fridays")
            .with_limit(5),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(hit_ids.len(), 5);
    assert_eq!(distinct(&hit_ids), 5);
    assert_eq!(hit_ids[0], paraphrase.id, "successor stands in at the top");
    assert!(!result.hits[0].version.successor_of.is_empty());
}

/// A contested (weak-link) thought stays visible, is preceded by its
/// contester, and neither crowds out the other live matches.
#[test]
fn test_contested_thought_does_not_starve_the_page() {
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "weak", StorageAdapterKind::Binary)
            .unwrap();
    add_filler(&mut chain);
    let old = chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Nightly build server status: use runner A.",
            ),
        )
        .unwrap()
        .clone();
    let fix = chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Correction,
                "Correction: the earlier runner choice was wrong, build on runner B.",
            )
            .with_refs(vec![old.index]),
        )
        .unwrap()
        .clone();
    assert_eq!(chain.version_status(old.id), VersionStatus::Contested);
    for i in 0..12 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Insight,
                    format!("Nightly build server status note {i}."),
                ),
            )
            .unwrap();
    }
    // A short page is still full of distinct live matches.
    let short = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY).with_limit(6));
    let short_ids: Vec<Uuid> = short.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(short_ids.len(), 6);
    assert_eq!(distinct(&short_ids), 6);
    // A page wide enough for every match holds the contested thought and the
    // contester that precedes it.
    let all = chain.query_ranked(&RankedSearchQuery::new().with_text(QUERY).with_limit(50));
    let hit_ids: Vec<Uuid> = all.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(hit_ids.len(), 14);
    assert_eq!(distinct(&hit_ids), 14);
    let old_pos = hit_ids
        .iter()
        .position(|id| *id == old.id)
        .expect("contested stays visible");
    let fix_pos = hit_ids
        .iter()
        .position(|id| *id == fix.id)
        .expect("contester promoted");
    assert!(
        fix_pos < old_pos,
        "contester precedes the contested thought"
    );
}

/// Context bundles pick `limit` distinct live seeds as well.
#[test]
fn test_context_bundles_are_not_starved() {
    let (_dir, chain, ids) = starved_chain();
    let bundles =
        chain.query_context_bundles(&RankedSearchQuery::new().with_text(QUERY).with_limit(5));
    assert_eq!(bundles.bundles.len(), 5);
    let seeds: Vec<Option<u64>> = bundles
        .bundles
        .iter()
        .map(|b| b.seed.locator.thought_index)
        .collect();
    let head = chain.get_thought_by_id(ids[VERSIONS - 1]).unwrap().index;
    assert_eq!(seeds[0], Some(head));
    assert_eq!(seeds.iter().collect::<HashSet<_>>().len(), 5);
}

/// A caller-supplied `limit = usize::MAX` must neither overflow nor allocate
/// for it: every live match comes back once, for ranked search and bundles.
#[test]
fn test_limit_usize_max_returns_every_live_match() {
    let (_dir, chain, ids) = starved_chain();
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_limit(usize::MAX),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(hit_ids.len(), LIVE_FACTS + 1);
    assert_eq!(distinct(&hit_ids), hit_ids.len());
    assert_eq!(hit_ids[0], ids[VERSIONS - 1]);
    let bundles = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_limit(usize::MAX),
    );
    assert_eq!(bundles.bundles.len(), LIVE_FACTS + 1);
}

/// Reranking keeps the page full of distinct live memories.
#[test]
fn test_reranking_with_lineage_fills_the_page() {
    let (_dir, chain, ids) = starved_chain();
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_limit(5)
            .with_reranking(50),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(hit_ids.len(), 5);
    assert_eq!(distinct(&hit_ids), 5);
    assert!(hit_ids.iter().all(|id| !ids[..VERSIONS - 1].contains(id)));
}

/// `as_of` reads the lineage as it stood: at the second version's timestamp the
/// live version is v2, v1 is superseded and v3 onward do not exist yet.
#[test]
fn test_as_of_reads_the_lineage_as_it_stood() {
    let (_dir, chain, ids) = starved_chain();
    let v2 = chain.get_thought_by_id(ids[1]).unwrap().timestamp;
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_limit(10)
            .with_as_of(v2),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert!(
        hit_ids.contains(&ids[1]),
        "v2 is the live version at its own timestamp"
    );
    assert!(!hit_ids.contains(&ids[0]), "v1 is superseded by v2 by then");
    assert!(
        hit_ids.iter().all(|id| !ids[2..].contains(id)),
        "later versions do not exist yet"
    );
}

/// A successor the request's filters exclude does not stand in: neither the
/// superseded version nor the filtered-out successor is returned.
#[test]
fn test_successor_excluded_by_the_filter_does_not_stand_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut chain = MentisDb::open_with_key_and_storage_kind(
        dir.path(),
        "filtered",
        StorageAdapterKind::Binary,
    )
    .unwrap();
    add_filler(&mut chain);
    let old = chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Nightly build server status: runner A.",
            ),
        )
        .unwrap()
        .clone();
    let successor = chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Insight, "Switched to runner B.")
                .with_relations(vec![rel(ThoughtRelationKind::Supersedes, &old)]),
        )
        .unwrap()
        .clone();
    let filter = mentisdb::ThoughtQuery::new().with_types(vec![ThoughtType::Decision]);
    let result = chain.query_ranked(
        &RankedSearchQuery::new()
            .with_text(QUERY)
            .with_filter(filter)
            .with_limit(5),
    );
    let hit_ids: Vec<Uuid> = result.hits.iter().map(|h| h.thought.id).collect();
    assert!(!hit_ids.contains(&old.id));
    assert!(!hit_ids.contains(&successor.id));
}
