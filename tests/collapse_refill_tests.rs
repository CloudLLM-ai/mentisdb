//! Refilling a page after near-duplicate collapsing reads a bounded ranked
//! window incrementally.
//!
//! Collapsing removes hits from a page, so the page is refilled from further
//! down the ranking. A refill used to start over (re-materialize and
//! re-tokenize the whole window) at every doubling, with no upper bound.

use mentisdb::{MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtType};
use uuid::Uuid;

fn chain(thoughts_of_noise: usize) -> (tempfile::TempDir, MentisDb) {
    // Lexical only: a large chain starts a background embedding-model load, and
    // a test process that exits while it runs crashes inside onnxruntime.
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "refill", StorageAdapterKind::Binary)
            .unwrap();
    // Keeps the query word under the document-frequency cutoff.
    for i in 0..thoughts_of_noise {
        chain
            .append_thought(
                "n",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!("wombat journal entry{i} burrow"),
                ),
            )
            .unwrap();
    }
    (dir, chain)
}

fn query(limit: usize) -> RankedSearchQuery {
    RankedSearchQuery::new()
        .with_text("quokka")
        .with_limit(limit)
        .with_collapse_duplicates(Some(0.9))
}

/// Many identical copies outrank the distinct matches; the page still fills
/// from the distinct ones, and the survivor lists every copy it absorbed.
#[test]
fn test_refill_reaches_distinct_matches_past_exact_copies() {
    let (_dir, mut c) = chain(3000);
    for _ in 0..300 {
        c.append_thought("a", ThoughtInput::new(ThoughtType::Finding, "quokka note"))
            .unwrap();
    }
    for i in 0..12 {
        c.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Finding,
                format!("quokka survey entry{i} habitat{i} diet{i} range{i}"),
            ),
        )
        .unwrap();
    }
    let hits = c.query_ranked(&query(10)).hits;
    assert_eq!(hits.len(), 10);
    assert_eq!(hits[0].thought.content, "quokka note");
    assert_eq!(hits[0].collapsed.len(), 299);
    let ids: std::collections::HashSet<Uuid> = hits.iter().map(|h| h.thought.id).collect();
    assert_eq!(ids.len(), 10);
}

/// The refill window is capped (4,096 slots for a short page): a page whose
/// distinct matches lie beyond it returns what the window held instead of
/// reading the whole ranking.
#[test]
fn test_refill_window_is_capped() {
    let (_dir, mut c) = chain(20_000);
    for _ in 0..4_500 {
        c.append_thought("a", ThoughtInput::new(ThoughtType::Finding, "quokka note"))
            .unwrap();
    }
    for i in 0..12 {
        c.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Finding,
                format!("quokka survey entry{i} habitat{i} diet{i} range{i}"),
            ),
        )
        .unwrap();
    }
    let hits = c.query_ranked(&query(10)).hits;
    assert!(!hits.is_empty() && hits.len() < 10, "got {}", hits.len());
    assert_eq!(hits[0].thought.content, "quokka note");
}

/// Collapsing is incremental, so a short page is a prefix of a long one over a
/// corpus of near-duplicate families.
#[test]
fn test_short_page_is_a_prefix_of_a_long_page_with_near_duplicates() {
    let (_dir, mut c) = chain(1500);
    for family in 0..40 {
        for variant in 0..3 {
            c.append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!(
                        "quokka survey family{family} habitat{family} diet{family} range{family} season{family} variant{}",
                        if variant == 0 { "x" } else { "y" }
                    ),
                ),
            )
            .unwrap();
        }
    }
    let long: Vec<Uuid> = c
        .query_ranked(&query(30))
        .hits
        .iter()
        .map(|h| h.thought.id)
        .collect();
    let short: Vec<Uuid> = c
        .query_ranked(&query(7))
        .hits
        .iter()
        .map(|h| h.thought.id)
        .collect();
    assert_eq!(short.len(), 7);
    assert_eq!(short[..], long[..7]);
    let distinct: std::collections::HashSet<_> = long.iter().collect();
    assert_eq!(distinct.len(), long.len());
}
