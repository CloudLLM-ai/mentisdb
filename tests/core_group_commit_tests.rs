//! Regression test: a lone durable append must not wait out the group-commit window.

use mentisdb::{MentisDb, ThoughtType};
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// A lone durable append must not wait out the group-commit window.
#[test]
fn test_single_durable_append_does_not_wait_for_group_commit_window() {
    // Read when the writer thread starts; set before any chain is opened.
    std::env::set_var("MENTISDB_GROUP_COMMIT_MS", "1000");
    let temp = tempdir().unwrap();
    let mut chain = MentisDb::open_with_key(temp.path(), "latency").unwrap();
    chain
        .append("agent", ThoughtType::Finding, "warm up")
        .unwrap();
    let started = Instant::now();
    chain.append("agent", ThoughtType::Finding, "lone").unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "single append took {:?}",
        started.elapsed()
    );
}
