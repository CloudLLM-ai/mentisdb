//! Tests for dedup's cached token window: results must match a fresh scan of
//! the last `dedup_scan_window` thoughts however the window moves.

use mentisdb::{MentisDb, ThoughtRelationKind, ThoughtType};
use tempfile::tempdir;
use uuid::Uuid;

const DUPLICATE: &str = "database migration rollback failed on the primary replica";
const FILLER: [&str; 4] = [
    "weekly lunch menu planning for the office",
    "quarterly budget review with finance team",
    "garden irrigation schedule for spring",
    "conference travel booking confirmation emails",
];

fn supersedes_target(chain: &mut MentisDb, content: &str) -> Option<Uuid> {
    let thought = chain.append("a", ThoughtType::Finding, content).unwrap();
    thought
        .relations
        .iter()
        .find(|relation| relation.kind == ThoughtRelationKind::Supersedes)
        .map(|relation| relation.target_id)
}

fn open_with_dedup(dir: &std::path::Path) -> MentisDb {
    let mut chain = MentisDb::open_with_key(dir, "dedup").unwrap();
    chain.with_dedup_threshold(Some(0.8));
    chain
}

/// A near duplicate inside the window supersedes the most similar thought
/// even after many unrelated appends slid the window forward.
#[test]
fn test_dedup_finds_duplicate_while_window_slides() {
    let dir = tempdir().unwrap();
    let mut chain = open_with_dedup(dir.path());
    chain.with_dedup_scan_window(8);
    let first = chain
        .append("a", ThoughtType::Finding, DUPLICATE)
        .unwrap()
        .id;
    for round in 0..6 {
        chain
            .append("a", ThoughtType::Finding, FILLER[round % 4])
            .unwrap();
    }
    assert_eq!(supersedes_target(&mut chain, DUPLICATE), Some(first));
}

/// A duplicate that scrolled out of a small window is not found; growing the
/// window again (which invalidates the cache) finds it once more.
#[test]
fn test_dedup_window_shrink_then_grow_matches_fresh_scan() {
    let dir = tempdir().unwrap();
    let mut chain = open_with_dedup(dir.path());
    let first = chain
        .append("a", ThoughtType::Finding, DUPLICATE)
        .unwrap()
        .id;
    for round in 0..5 {
        chain
            .append("a", ThoughtType::Finding, FILLER[round % 4])
            .unwrap();
    }
    chain.with_dedup_scan_window(2);
    assert_eq!(
        supersedes_target(
            &mut chain,
            "weekly lunch menu planning for the office staff"
        ),
        Some(chain.thoughts()[5].id),
        "the in-window filler is the best match"
    );
    assert_eq!(supersedes_target(&mut chain, DUPLICATE), None);
    chain.with_dedup_scan_window(64);
    assert_eq!(
        supersedes_target(&mut chain, DUPLICATE),
        Some(first),
        "grown window sees the older duplicate again (earliest wins ties)"
    );
}

/// Reopening a chain starts with an empty cache that fills from the stored
/// thoughts, so duplicates are still found after a restart.
#[test]
fn test_dedup_cache_rebuilds_after_reopen() {
    let dir = tempdir().unwrap();
    let first = {
        let mut chain = open_with_dedup(dir.path());
        chain
            .append("a", ThoughtType::Finding, DUPLICATE)
            .unwrap()
            .id
    };
    let mut chain = open_with_dedup(dir.path());
    assert_eq!(supersedes_target(&mut chain, DUPLICATE), Some(first));
}
