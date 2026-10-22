//! Regression tests for `MentisDb::branch_from` argument validation and
//! ancestor discovery.

use mentisdb::{load_registered_chains, MentisDb, ThoughtType};
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

/// Ancestors are discovered by peeking genesis records: the full lineage is
/// returned nearest-first and no chain is opened (or registered) as a side
/// effect, even for a dangling parent reference.
#[test]
fn test_ancestor_chain_keys_walks_lineage_without_opening_chains() {
    let dir = tempdir().unwrap();
    let mut root = MentisDb::open_with_key(dir.path(), "root").unwrap();
    let root_id = root.append("a", ThoughtType::Decision, "base").unwrap().id;
    let root_file = std::path::PathBuf::from(root.storage_location());
    root.flush().unwrap();
    drop(root);
    let middle = MentisDb::branch_from(dir.path(), "root", root_id, "middle").unwrap();
    let middle_id = middle.thoughts()[0].id;
    drop(middle);
    let leaf = MentisDb::branch_from(dir.path(), "middle", middle_id, "leaf").unwrap();

    let registered_before = load_registered_chains(dir.path()).unwrap().chains.len();
    assert_eq!(leaf.ancestor_chain_keys(), vec!["middle", "root"]);
    assert_eq!(
        load_registered_chains(dir.path()).unwrap().chains.len(),
        registered_before
    );

    std::fs::remove_file(&root_file).unwrap();
    assert_eq!(leaf.ancestor_chain_keys(), vec!["middle", "root"]);
    assert!(!root_file.exists(), "missing parent not recreated");
}
