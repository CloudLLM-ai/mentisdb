//! A thought whose record would exceed the 10 MiB per-record cap is refused at
//! append, before anything is written.
//!
//! Regression: the cap was enforced only when a chain was loaded. An append
//! over it succeeded, and the next open of the chain failed with "binary
//! thought payload length ... exceeds maximum 10485760", so one oversized
//! thought made the whole chain unreadable. Ordinary REST and MCP bodies are
//! capped at 2 MiB, so the way in was the library itself and the large-body
//! routes (`/v1/import-markdown`, `/v1/extract-memories`, `/v1/skills/upload`,
//! up to `MENTISDB_MAX_BODY_BYTES`, 64 MiB by default, for a token holder).

use mentisdb::{MentisDb, ThoughtInput, ThoughtType};

/// Content just over the record cap (10 MiB plus the other fields).
fn oversized() -> String {
    "x".repeat(10 * 1024 * 1024 + 1)
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_size_limit_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The oversized append fails with `InvalidInput`; the chain keeps working,
/// and reopens with exactly the thoughts that were accepted.
#[test]
fn an_oversized_thought_is_refused_and_the_chain_still_opens() {
    let dir = temp_dir("lib");
    {
        let mut db = MentisDb::open_with_key(&dir, "c").unwrap();
        db.append_thought("a", ThoughtInput::new(ThoughtType::Summary, "before"))
            .unwrap();
        let err = match db.append_thought("a", ThoughtInput::new(ThoughtType::Summary, oversized()))
        {
            // Never print the thought itself: it is 10 MiB.
            Ok(t) => panic!("a {}-byte thought was accepted", t.content.len()),
            Err(e) => e,
        };
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
        assert!(err.to_string().contains("too large"), "{err}");
        assert_eq!(db.thoughts().len(), 1, "nothing was added in memory");
        db.append_thought("a", ThoughtInput::new(ThoughtType::Summary, "after"))
            .unwrap();
    }
    let reopened = MentisDb::open_with_key(&dir, "c").expect("the chain still opens");
    let contents: Vec<&str> = reopened
        .thoughts()
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(contents, ["before", "after"]);
    let _ = std::fs::remove_dir_all(&dir);
}
