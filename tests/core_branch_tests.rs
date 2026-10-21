//! Regression tests for `MentisDb::branch_from` argument validation.

use mentisdb::{MentisDb, ThoughtType};
use tempfile::tempdir;

/// Branching into a key that already holds thoughts must be rejected rather
/// than appending a second genesis-style thought to an unrelated chain.
#[test]
fn test_branch_from_rejects_non_empty_branch_chain() {
    let dir = tempdir().unwrap();
    let mut source = MentisDb::open_with_key(dir.path(), "main").unwrap();
    let id = source
        .append("a", ThoughtType::Decision, "plan A")
        .unwrap()
        .id;
    drop(source);

    MentisDb::branch_from(dir.path(), "main", id, "exp").unwrap();
    let Err(error) = MentisDb::branch_from(dir.path(), "main", id, "exp") else {
        panic!("second branch must fail");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);

    let branch = MentisDb::open_with_key(dir.path(), "exp").unwrap();
    assert_eq!(branch.thoughts().len(), 1, "existing branch is untouched");
}
