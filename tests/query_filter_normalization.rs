//! `source_episode` is normalised like `entity_type`: trimmed at append (blank
//! becomes `None`) and trimmed in the filter, so `"X "` finds `"X"`.
use mentisdb::{MentisDb, ThoughtInput, ThoughtQuery, ThoughtType};

fn contents(chain: &MentisDb, query: &ThoughtQuery) -> Vec<String> {
    chain
        .query(query)
        .into_iter()
        .map(|thought| thought.content.clone())
        .collect()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_query_filter_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// A trailing-space filter finds the stored value, matching how `entity_type`
/// is normalised, and a value appended with padding is stored trimmed.
#[test]
fn source_episode_is_normalised_like_entity_type() {
    let dir = temp_dir("norm");
    let mut chain = MentisDb::open(&dir, "a1", "A", None, None).expect("open");
    chain
        .append_thought(
            "a1",
            ThoughtInput::new(ThoughtType::Finding, "padded").with_source_episode("  X \t"),
        )
        .unwrap();
    chain
        .append_thought(
            "a1",
            ThoughtInput::new(ThoughtType::Finding, "blank").with_source_episode("   "),
        )
        .unwrap();
    let stored: Vec<Option<String>> = chain
        .thoughts()
        .iter()
        .map(|t| t.source_episode.clone())
        .collect();
    assert_eq!(stored, [Some("X".to_string()), None]);
    for filter in ["X", "X ", " X", "  X  "] {
        assert_eq!(
            contents(&chain, &ThoughtQuery::new().with_source_episode(filter)),
            ["padded"],
            "filter {filter:?}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// The `entity_type` filter is trimmed like the stored value (trimmed at
/// append), alone and combined with an indexed filter.
#[test]
fn entity_type_filter_is_trimmed_like_the_stored_value() {
    let dir = temp_dir("entity");
    let mut chain = MentisDb::open(&dir, "a1", "A", None, None).expect("open");
    chain
        .append_thought(
            "a1",
            ThoughtInput::new(ThoughtType::Finding, "padded")
                .with_entity_type(" person\t")
                .with_tags(["t"]),
        )
        .unwrap();
    chain
        .append_thought(
            "a1",
            ThoughtInput::new(ThoughtType::Finding, "other")
                .with_entity_type("place")
                .with_tags(["t"]),
        )
        .unwrap();
    for filter in ["person", "person ", " person", "  person\t"] {
        assert_eq!(
            contents(&chain, &ThoughtQuery::new().with_entity_type(filter)),
            ["padded"],
            "filter {filter:?}"
        );
        assert_eq!(
            contents(
                &chain,
                &ThoughtQuery::new()
                    .with_entity_type(filter)
                    .with_tags_any(["t"])
            ),
            ["padded"],
            "indexed filter {filter:?}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}
