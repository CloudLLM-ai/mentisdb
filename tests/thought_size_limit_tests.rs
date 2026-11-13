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

/// The early reject fires on the variable-length fields alone, before
/// signature verification: an oversized thought carrying a signature with no
/// key id fails as `too large`, not with the access error verification would
/// raise. This pins that the cheap check runs first.
#[test]
fn an_oversized_thought_is_refused_before_signature_checks() {
    let dir = temp_dir("early");
    let mut db = MentisDb::open_with_key(&dir, "c").unwrap();
    let mut input = ThoughtInput::new(ThoughtType::Summary, oversized());
    input.thought_signature = Some(vec![1, 2, 3]);
    let err = match db.append_thought("a", input) {
        Ok(t) => panic!("a {}-byte thought was accepted", t.content.len()),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert!(err.to_string().contains("too large"), "{err}");
    assert!(db.thoughts().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The early reject is only a lower bound: a thought just under the cap is
/// still accepted (content alone below the limit passes it), and one whose size
/// comes from tags alone, which the early reject does not count, is still
/// refused by the storage backstop with the same error.
#[test]
fn the_early_reject_is_a_lower_bound_and_the_backstop_still_applies() {
    let dir = temp_dir("bound");
    let mut db = MentisDb::open_with_key(&dir, "c").unwrap();
    db.append_thought(
        "a",
        ThoughtInput::new(ThoughtType::Summary, "x".repeat(9 * 1024 * 1024)),
    )
    .expect("9 MiB of content fits");
    let heavy_tags = ThoughtInput::new(ThoughtType::Summary, "small")
        .with_tags((0..11).map(|i| format!("{i}{}", "t".repeat(1024 * 1024))));
    let err = match db.append_thought("a", heavy_tags) {
        Ok(_) => panic!("11 MiB of tags was accepted"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    assert!(err.to_string().contains("too large"), "{err}");
    assert_eq!(db.thoughts().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The early reject must stay a strict lower bound of the stored record:
/// inputs whose raw size is far over the cap but whose stored record is just
/// under it are accepted. Heavy duplicate tags and concepts (normalisation
/// keeps one of each), whitespace padding around `entity_type` and
/// `source_episode` (trimmed at append), many references and a content just
/// under the cap together would trip a floor that counted raw sizes.
#[test]
fn the_early_reject_never_counts_more_than_the_stored_record() {
    let dir = temp_dir("floor");
    let mut db = MentisDb::open_with_key(&dir, "c").unwrap();
    for i in 0..20 {
        db.append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Summary, format!("p{i}")),
        )
        .unwrap();
    }
    let padding = " ".repeat(2 * 1024 * 1024);
    let input = ThoughtInput::new(
        ThoughtType::Summary,
        "x".repeat(10 * 1024 * 1024 - 16 * 1024),
    )
    .with_tags(std::iter::repeat_n("dup-tag".to_string(), 200_000))
    .with_concepts(std::iter::repeat_n("dup-concept".to_string(), 200_000))
    .with_entity_type(format!("{padding}person{padding}"))
    .with_source_episode(format!("{padding}episode{padding}"))
    .with_refs((0..20).collect());

    let stored = db
        .append_thought("a", input)
        .expect("a record just under the cap is accepted")
        .clone();

    assert_eq!(stored.tags, ["dup-tag"]);
    assert_eq!(stored.entity_type.as_deref(), Some("person"));
    assert_eq!(stored.source_episode.as_deref(), Some("episode"));
    assert_eq!(db.thoughts().len(), 21);
    drop(db);
    let reopened = MentisDb::open_with_key(&dir, "c").expect("the chain still opens");
    assert_eq!(reopened.thoughts().len(), 21);
    let _ = std::fs::remove_dir_all(&dir);
}
