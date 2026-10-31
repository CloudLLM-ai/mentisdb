//! Regression tests for the automatic-dedup supersession guard.
//!
//! Auto-dedup retires the most similar recent thought with a `Supersedes`
//! edge. It used to do so whatever the importance of the two thoughts, so a
//! near-copy filed as a throwaway could hide a thought the author rated as
//! critical. A prior thought that is more important than the new one is now
//! never retired.

use mentisdb::{MentisDb, ThoughtInput, ThoughtRelationKind, ThoughtType};
use tempfile::tempdir;

const TEXT: &str = "always run make clippy before marking the work complete";

fn chain() -> (tempfile::TempDir, MentisDb) {
    let dir = tempdir().unwrap();
    let mut db = MentisDb::open_with_key(dir.path(), "dedup-guard").unwrap();
    db.with_dedup_threshold(Some(0.85));
    (dir, db)
}

fn supersedes(db: &MentisDb, index: usize) -> usize {
    db.thoughts()[index]
        .relations
        .iter()
        .filter(|r| r.kind == ThoughtRelationKind::Supersedes)
        .count()
}

/// A same-importance near-copy still retires the older thought: the guard
/// must not turn dedup off.
#[test]
fn test_equal_importance_duplicate_still_supersedes() {
    let (_dir, mut db) = chain();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Constraint, TEXT).with_importance(0.9),
    )
    .unwrap();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Constraint, TEXT).with_importance(0.9),
    )
    .unwrap();
    assert_eq!(supersedes(&db, 1), 1);
    assert!(db.invalidated_thought_ids().contains(&db.thoughts()[0].id));
}

/// A less important near-copy must not retire a more important original.
#[test]
fn test_lower_importance_duplicate_does_not_supersede_higher() {
    let (_dir, mut db) = chain();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Constraint, TEXT).with_importance(0.95),
    )
    .unwrap();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Summary, TEXT).with_importance(0.3),
    )
    .unwrap();
    assert_eq!(supersedes(&db, 1), 0);
    assert!(db.invalidated_thought_ids().is_empty());
}

/// A more important near-copy may retire a less important original, and when
/// both a high and a low candidate exist the low one is the target.
#[test]
fn test_guard_skips_higher_candidate_and_picks_lower_one() {
    let (_dir, mut db) = chain();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Constraint, TEXT).with_importance(0.95),
    )
    .unwrap();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Finding, "unrelated filler about databases"),
    )
    .unwrap();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Finding, TEXT).with_importance(0.2),
    )
    .unwrap();
    // idx 2 is a less important copy of idx 0: guard leaves idx 0 alone.
    assert_eq!(supersedes(&db, 2), 0);
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Decision, TEXT).with_importance(0.6),
    )
    .unwrap();
    // idx 3 outranks idx 2 (0.2) but not idx 0 (0.95): it retires idx 2 only.
    let target = db.thoughts()[3]
        .relations
        .iter()
        .find(|r| r.kind == ThoughtRelationKind::Supersedes)
        .map(|r| r.target_id);
    assert_eq!(target, Some(db.thoughts()[2].id));
}
