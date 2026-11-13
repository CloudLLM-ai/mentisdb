//! `source_episode` and `entity_type` must keep filtering when they are
//! combined with an index-backed filter (tags, concepts, types, roles, agent
//! ids). Before the fix `MentisDb::query` narrowed candidates through the
//! indexes and then applied only the importance/confidence/time checks, so the
//! two exact-match fields were silently dropped and the extra thoughts leaked
//! into the result.
use mentisdb::{MentisDb, ThoughtInput, ThoughtQuery, ThoughtRole, ThoughtType};
use std::path::PathBuf;

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_query_filter_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

/// Two Summary thoughts by two agents, both tagged `t` / concept `c`, with
/// distinct episodes and entity types, plus one thought with neither field.
fn seeded(tag: &str) -> (MentisDb, PathBuf) {
    let dir = temp_dir(tag);
    let mut chain = MentisDb::open(&dir, "a1", "A", None, None).expect("open");
    let base = |content: &str| {
        ThoughtInput::new(ThoughtType::Summary, content)
            .with_tags(["t"])
            .with_concepts(["c"])
    };
    chain
        .append_thought(
            "a1",
            base("one")
                .with_source_episode("E1")
                .with_entity_type("alpha"),
        )
        .unwrap();
    chain
        .append_thought(
            "a2",
            base("two")
                .with_source_episode("E2")
                .with_entity_type("beta"),
        )
        .unwrap();
    chain.append_thought("a1", base("three")).unwrap();
    (chain, dir)
}

fn contents(chain: &MentisDb, query: &ThoughtQuery) -> Vec<String> {
    chain
        .query(query)
        .into_iter()
        .map(|thought| thought.content.clone())
        .collect()
}

/// The reviewer's repro: the episode filter alone gives one thought and must
/// still give one once `tags_any` joins it.
#[test]
fn source_episode_survives_tags_any() {
    let (chain, dir) = seeded("ep_tags");
    let episode = ThoughtQuery::new().with_source_episode("E1");
    assert_eq!(contents(&chain, &episode), ["one"]);
    assert_eq!(
        contents(&chain, &episode.clone().with_tags_any(["t"])),
        ["one"]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn source_episode_survives_every_index_filter() {
    let (chain, dir) = seeded("ep_all");
    let episode = ThoughtQuery::new().with_source_episode("E2");
    let cases = [
        episode.clone().with_concepts_any(["c"]),
        episode.clone().with_types(vec![ThoughtType::Summary]),
        episode.clone().with_roles(vec![ThoughtRole::Memory]),
        episode.clone().with_agent_ids(["a2"]),
        episode.clone().with_agent_ids(["a1", "a2"]),
    ];
    for query in &cases {
        assert_eq!(contents(&chain, query), ["two"], "{query:?}");
    }
    // An index match with the wrong episode must give nothing.
    let wrong = ThoughtQuery::new()
        .with_source_episode("E2")
        .with_agent_ids(["a1"]);
    assert!(contents(&chain, &wrong).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn entity_type_survives_every_index_filter() {
    let (chain, dir) = seeded("et_all");
    let entity = ThoughtQuery::new().with_entity_type("beta");
    let cases = [
        entity.clone().with_tags_any(["t"]),
        entity.clone().with_concepts_any(["c"]),
        entity.clone().with_types(vec![ThoughtType::Summary]),
        entity.clone().with_roles(vec![ThoughtRole::Memory]),
        entity.clone().with_agent_ids(["a2"]),
    ];
    for query in &cases {
        assert_eq!(contents(&chain, query), ["two"], "{query:?}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn entity_type_and_source_episode_combine_with_an_index_filter() {
    let (chain, dir) = seeded("both");
    let hit = ThoughtQuery::new()
        .with_tags_any(["t"])
        .with_entity_type("alpha")
        .with_source_episode("E1");
    assert_eq!(contents(&chain, &hit), ["one"]);
    let miss = ThoughtQuery::new()
        .with_tags_any(["t"])
        .with_entity_type("alpha")
        .with_source_episode("E2");
    assert!(contents(&chain, &miss).is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

/// Traversal shares the indexed-candidate path and must apply the fields too.
#[test]
fn traversal_applies_source_episode_with_an_index_filter() {
    use mentisdb::{ThoughtTraversalAnchor, ThoughtTraversalDirection, ThoughtTraversalRequest};
    let (chain, dir) = seeded("trav");
    let request = ThoughtTraversalRequest::new(
        ThoughtTraversalAnchor::Genesis,
        ThoughtTraversalDirection::Forward,
        10,
    )
    .with_filter(
        ThoughtQuery::new()
            .with_tags_any(["t"])
            .with_source_episode("E2"),
    );
    let page = chain.traverse_thoughts(&request).expect("traverse");
    let got: Vec<&str> = page.thoughts.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(got, ["two"]);
    let _ = std::fs::remove_dir_all(dir);
}
