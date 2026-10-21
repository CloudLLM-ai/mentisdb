//! Pins point-in-time invalidation semantics backed by the per-target edge index.

use chrono::Duration;
use mentisdb::{MentisDb, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType};
use tempfile::tempdir;

fn supersede(target: uuid::Uuid) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Correction, "newer").with_relations(vec![ThoughtRelation {
        kind: ThoughtRelationKind::Supersedes,
        target_id: target,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }])
}

/// A thought is visible as of a time before its superseding thought, hidden
/// afterwards, both for edges added live and for edges loaded at open.
#[test]
fn test_is_invalidated_as_of_respects_edge_timestamps_live_and_after_reopen() {
    let dir = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key(dir.path(), "inval").unwrap();
    let old = chain
        .append("a", ThoughtType::Finding, "older")
        .unwrap()
        .clone();
    let unrelated = chain.append("a", ThoughtType::Finding, "other").unwrap().id;
    let newer = chain
        .append_thought("a", supersede(old.id))
        .unwrap()
        .clone();

    let before = newer.timestamp - Duration::seconds(1);
    let after = newer.timestamp + Duration::seconds(1);
    let check = |chain: &MentisDb| {
        assert!(!chain.is_invalidated_as_of(old.id, before));
        assert!(chain.is_invalidated_as_of(old.id, after));
        assert!(!chain.is_invalidated_as_of(unrelated, after));
        assert!(!chain.is_invalidated_as_of(newer.id, after));
    };
    check(&chain);
    chain.flush().unwrap();
    drop(chain);
    check(&MentisDb::open_with_key(dir.path(), "inval").unwrap());
}
