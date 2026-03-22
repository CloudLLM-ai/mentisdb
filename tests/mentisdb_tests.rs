use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use mentisdb::{
    chain_filename, chain_key_from_storage_filename, chain_storage_filename,
    load_registered_chains, migrate_registered_chains, migrate_registered_chains_with_adapter,
    signable_thought_payload, AgentStatus, BinaryStorageAdapter, JsonlStorageAdapter, MentisDb,
    PublicKeyAlgorithm, StorageAdapter, StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery,
    ThoughtRelation, ThoughtRelationKind, ThoughtRole, ThoughtTimeWindow, ThoughtTraversalAnchor,
    ThoughtTraversalDirection, ThoughtTraversalRequest, ThoughtType, TimeWindowUnit,
    FLUSH_THRESHOLD, MENTISDB_CURRENT_VERSION,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyThoughtV0Record {
    id: Uuid,
    index: u64,
    timestamp: chrono::DateTime<chrono::Utc>,
    session_id: Option<Uuid>,
    agent_id: String,
    agent_name: String,
    agent_owner: Option<String>,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: String,
    confidence: Option<f32>,
    importance: f32,
    tags: Vec<String>,
    concepts: Vec<String>,
    refs: Vec<u64>,
    relations: Vec<ThoughtRelation>,
    prev_hash: String,
    hash: String,
}

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("thoughtchain_test_{}_{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn append_test_thought(
    chain: &mut MentisDb,
    agent_id: &str,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: &str,
) -> Thought {
    chain
        .append_thought(
            agent_id,
            ThoughtInput::new(thought_type, content)
                .with_agent_name(agent_id)
                .with_role(role),
        )
        .unwrap()
        .clone()
}

#[test]
fn append_and_reload_preserves_semantic_metadata() {
    let dir = unique_chain_dir();
    let session_id = Uuid::new_v4();

    {
        let mut chain = MentisDb::open(&dir, "agent1", "Analyst", Some("rust"), None).unwrap();
        chain
            .append_thought(
                "agent1",
                ThoughtInput::new(
                    ThoughtType::Insight,
                    "The bottleneck is cache invalidation.",
                )
                .with_session_id(session_id)
                .with_agent_name("Analyst")
                .with_agent_owner("cloudllm")
                .with_importance(0.95)
                .with_confidence(0.8)
                .with_tags(["performance", "cache"])
                .with_concepts(["latency", "cache invalidation"]),
            )
            .unwrap();
    }

    let chain = MentisDb::open(
        &dir,
        "agent1",
        "Analyst",
        Some("different"),
        Some("changed"),
    )
    .unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    let thought = &chain.thoughts()[0];
    assert_eq!(thought.session_id, Some(session_id));
    assert_eq!(thought.thought_type, ThoughtType::Insight);
    assert_eq!(thought.role, ThoughtRole::Memory);
    assert_eq!(thought.agent_id, "agent1");
    let record = chain.agent_registry().agents.get("agent1").unwrap();
    assert_eq!(record.display_name, "Analyst");
    assert_eq!(record.owner.as_deref(), Some("cloudllm"));
    assert_eq!(thought.tags, vec!["performance", "cache"]);
    assert_eq!(thought.concepts, vec!["latency", "cache invalidation"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resolve_context_follows_refs_and_relations() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent1", "Analyst", Some("data"), None).unwrap();

    let base_id = chain
        .append(
            "agent1",
            ThoughtType::FactLearned,
            "The dataset has 4 million rows.",
        )
        .unwrap()
        .id;
    chain
        .append_thought(
            "agent1",
            ThoughtInput::new(
                ThoughtType::Hypothesis,
                "Failures may come from stale partitions.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: base_id,
            }]),
        )
        .unwrap();
    chain
        .append_with_refs(
            "agent1",
            ThoughtType::Summary,
            "Important memory snapshot",
            vec![1],
        )
        .unwrap();

    let resolved = chain.resolve_context(2);
    let indices: Vec<u64> = resolved.iter().map(|thought| thought.index).collect();
    assert_eq!(indices, vec![0, 1, 2]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_filters_by_type_tag_and_text() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent1", "Analyst", Some("memory"), None).unwrap();

    chain
        .append_thought(
            "agent1",
            ThoughtInput::new(
                ThoughtType::Constraint,
                "Memory must survive session resets.",
            )
            .with_importance(0.9)
            .with_tags(["durability"])
            .with_concepts(["persistence"]),
        )
        .unwrap();
    chain
        .append_thought(
            "agent1",
            ThoughtInput::new(ThoughtType::Idea, "Consider vector search later.")
                .with_importance(0.4)
                .with_tags(["retrieval"]),
        )
        .unwrap();

    let results = chain.query(
        &ThoughtQuery::new()
            .with_types(vec![ThoughtType::Constraint])
            .with_tags_any(["durability"])
            .with_text("survive"),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].thought_type, ThoughtType::Constraint);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_filters_by_partial_tag_and_concept_matches() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "query-tag-concept-substrings").unwrap();

    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Insight, "Keep durable storage append-only.")
                .with_tags(["durability", "storage"])
                .with_concepts(["search traversal"]),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "Defer vector indexing.")
                .with_tags(["retrieval"])
                .with_concepts(["ranking"]),
        )
        .unwrap();

    let results = chain.query(
        &ThoughtQuery::new()
            .with_tags_any(["durab"])
            .with_concepts_any(["traver"]),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content, "Keep durable storage append-only.");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_filters_retrospectives_and_lesson_learned() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "shared-project").unwrap();

    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::LessonLearned,
                "When native tool calls return multiple tool invocations, resolve all of them before the next model round-trip.",
            )
            .with_agent_name("Astro")
            .with_role(ThoughtRole::Retrospective)
            .with_tags(["tools", "openai"])
            .with_concepts(["multi-tool call handling"]),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Keep the shared MCP runtime in the standalone mcp crate.",
            )
            .with_agent_name("Astro"),
        )
        .unwrap();

    let results = chain.query(
        &ThoughtQuery::new()
            .with_types(vec![ThoughtType::LessonLearned])
            .with_roles(vec![ThoughtRole::Retrospective]),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].thought_type, ThoughtType::LessonLearned);
    assert_eq!(results[0].role, ThoughtRole::Retrospective);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn query_filters_by_timestamp_window() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent1", "Analyst", Some("timing"), None).unwrap();

    let first_timestamp = chain
        .append("agent1", ThoughtType::Insight, "First observation.")
        .unwrap()
        .timestamp;
    sleep(Duration::from_millis(5));
    let second_timestamp = chain
        .append("agent1", ThoughtType::Insight, "Second observation.")
        .unwrap()
        .timestamp;
    sleep(Duration::from_millis(5));
    let third_timestamp = chain
        .append("agent1", ThoughtType::Insight, "Third observation.")
        .unwrap()
        .timestamp;

    assert!(first_timestamp <= second_timestamp);
    assert!(second_timestamp <= third_timestamp);

    let middle = chain.query(
        &ThoughtQuery::new()
            .with_since(second_timestamp)
            .with_until(second_timestamp),
    );
    assert_eq!(middle.len(), 1);
    assert_eq!(middle[0].content, "Second observation.");

    let trailing = chain.query(&ThoughtQuery::new().with_since(second_timestamp));
    assert_eq!(trailing.len(), 2);
    assert_eq!(trailing[0].content, "Second observation.");
    assert_eq!(trailing[1].content, "Third observation.");

    let leading = chain.query(&ThoughtQuery::new().with_until(second_timestamp));
    assert_eq!(leading.len(), 2);
    assert_eq!(leading[0].content, "First observation.");
    assert_eq!(leading[1].content, "Second observation.");

    let empty = chain.query(
        &ThoughtQuery::new()
            .with_since(third_timestamp)
            .with_until(first_timestamp),
    );
    assert!(empty.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registered_chain_counts_batch_append_updates_until_threshold_or_drop() {
    let dir = unique_chain_dir();
    let chain_key = "batched-chain-registration";

    {
        let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        chain.append("astro", ThoughtType::Insight, "t0").unwrap();

        let first = load_registered_chains(&dir).unwrap();
        let first_entry = first.chains.get(chain_key).unwrap();
        assert_eq!(first_entry.thought_count, 1);
        assert_eq!(first_entry.agent_count, 1);

        for index in 1..FLUSH_THRESHOLD {
            chain
                .append("astro", ThoughtType::Insight, &format!("t{index}"))
                .unwrap();
        }

        let before_threshold = load_registered_chains(&dir).unwrap();
        assert_eq!(
            before_threshold
                .chains
                .get(chain_key)
                .unwrap()
                .thought_count,
            1
        );

        chain
            .append(
                "astro",
                ThoughtType::Insight,
                &format!("t{}", FLUSH_THRESHOLD),
            )
            .unwrap();

        let after_threshold = load_registered_chains(&dir).unwrap();
        assert_eq!(
            after_threshold.chains.get(chain_key).unwrap().thought_count,
            (FLUSH_THRESHOLD + 1) as u64
        );

        chain.append("astro", ThoughtType::Insight, "tail").unwrap();
        let before_drop = load_registered_chains(&dir).unwrap();
        assert_eq!(
            before_drop.chains.get(chain_key).unwrap().thought_count,
            (FLUSH_THRESHOLD + 1) as u64
        );
    }

    let after_drop = load_registered_chains(&dir).unwrap();
    let entry = after_drop.chains.get(chain_key).unwrap();
    assert_eq!(entry.thought_count, (FLUSH_THRESHOLD + 2) as u64);
    assert_eq!(entry.agent_count, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registered_chain_counts_flush_immediately_when_agent_count_changes() {
    let dir = unique_chain_dir();
    let chain_key = "agent-count-registration";
    let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();

    chain
        .append("astro", ThoughtType::Insight, "astro thought")
        .unwrap();
    chain
        .append("apollo", ThoughtType::Insight, "apollo thought")
        .unwrap();

    let registry = load_registered_chains(&dir).unwrap();
    let entry = registry.chains.get(chain_key).unwrap();
    assert_eq!(entry.thought_count, 2);
    assert_eq!(entry.agent_count, 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn get_thought_by_id_hash_and_index_returns_expected_records() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "lookup-demo").unwrap();

    let first = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "First lookup thought.",
    );
    let second = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Memory,
        "Second lookup thought.",
    );

    assert_eq!(
        chain.get_thought_by_id(first.id).unwrap().index,
        first.index
    );
    assert_eq!(
        chain.get_thought_by_hash(&second.hash).unwrap().id,
        second.id
    );
    assert_eq!(chain.get_thought_by_index(1).unwrap().hash, second.hash);

    assert!(chain.get_thought_by_id(Uuid::new_v4()).is_none());
    assert!(chain.get_thought_by_hash("missing-hash").is_none());
    assert!(chain.get_thought_by_index(99).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn genesis_and_head_thought_return_first_and_last_records() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "head-genesis").unwrap();
    assert!(chain.genesis_thought().is_none());
    assert!(chain.head_thought().is_none());

    let first = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "Genesis thought.",
    );
    let last = append_test_thought(
        &mut chain,
        "apollo",
        ThoughtType::Summary,
        ThoughtRole::Checkpoint,
        "Head thought.",
    );

    assert_eq!(chain.genesis_thought().unwrap().id, first.id);
    assert_eq!(chain.head_thought().unwrap().id, last.id);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_moves_forward_from_anchor_in_chunks() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-forward").unwrap();
    let thoughts = [
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t0",
        ),
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t1",
        ),
        append_test_thought(
            &mut chain,
            "apollo",
            ThoughtType::Decision,
            ThoughtRole::Memory,
            "t2",
        ),
        append_test_thought(
            &mut chain,
            "apollo",
            ThoughtType::Decision,
            ThoughtRole::Memory,
            "t3",
        ),
    ];

    let page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[1].id),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();

    let indexes: Vec<u64> = page.thoughts.iter().map(|thought| thought.index).collect();
    assert_eq!(indexes, vec![2, 3]);
    assert!(!page.has_more);
    assert_eq!(
        page.next_cursor.as_ref().map(|cursor| cursor.index),
        Some(3)
    );
    assert_eq!(
        page.previous_cursor.as_ref().map(|cursor| cursor.index),
        Some(2)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_moves_backward_from_anchor_in_chunks() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-backward").unwrap();
    let thoughts = [
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t0",
        ),
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t1",
        ),
        append_test_thought(
            &mut chain,
            "apollo",
            ThoughtType::Decision,
            ThoughtRole::Memory,
            "t2",
        ),
        append_test_thought(
            &mut chain,
            "apollo",
            ThoughtType::Decision,
            ThoughtRole::Memory,
            "t3",
        ),
    ];

    let page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[3].id),
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: false,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();

    let indexes: Vec<u64> = page.thoughts.iter().map(|thought| thought.index).collect();
    assert_eq!(indexes, vec![2, 1]);
    assert!(page.has_more);
    assert_eq!(
        page.next_cursor.as_ref().map(|cursor| cursor.index),
        Some(1)
    );
    assert_eq!(
        page.previous_cursor.as_ref().map(|cursor| cursor.index),
        Some(2)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_can_include_anchor() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-anchor").unwrap();
    let thoughts = [
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t0",
        ),
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Decision,
            ThoughtRole::Checkpoint,
            "t1",
        ),
        append_test_thought(
            &mut chain,
            "apollo",
            ThoughtType::Summary,
            ThoughtRole::Checkpoint,
            "t2",
        ),
    ];

    let forward = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[1].id),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(forward.thoughts[0].index, 1);

    let backward = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[1].id),
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(backward.thoughts[0].index, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_filters_by_agent_type_role_and_time_window() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-filtered").unwrap();

    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "old astro checkpoint",
    );
    sleep(Duration::from_millis(5));
    let start = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "matching astro checkpoint",
    )
    .timestamp;
    sleep(Duration::from_millis(5));
    append_test_thought(
        &mut chain,
        "apollo",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "wrong agent checkpoint",
    );
    sleep(Duration::from_millis(5));
    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Checkpoint,
        "wrong type checkpoint",
    );
    sleep(Duration::from_millis(5));
    let end = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "second matching astro checkpoint",
    )
    .timestamp;

    let page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 4,
            filter: ThoughtQuery::new()
                .with_agent_ids(["astro"])
                .with_types(vec![ThoughtType::Decision])
                .with_roles(vec![ThoughtRole::Checkpoint])
                .with_since(start)
                .with_until(end),
        })
        .unwrap();

    let contents: Vec<&str> = page
        .thoughts
        .iter()
        .map(|thought| thought.content.as_str())
        .collect();
    assert_eq!(
        contents,
        vec![
            "matching astro checkpoint",
            "second matching astro checkpoint"
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_filters_by_tag_and_concept_in_indexed_order() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-tag-concept").unwrap();

    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "non-matching anchor",
    );
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "first indexed match")
                .with_role(ThoughtRole::Checkpoint)
                .with_tags(["durability"])
                .with_concepts(["search traversal"]),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "second indexed match")
                .with_role(ThoughtRole::Checkpoint)
                .with_tags(["durable"])
                .with_concepts(["traversal tuning"]),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Decision, "non-matching tail")
                .with_role(ThoughtRole::Checkpoint)
                .with_tags(["retrieval"])
                .with_concepts(["ranking"]),
        )
        .unwrap();

    let page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 1,
            filter: ThoughtQuery::new()
                .with_tags_any(["durab"])
                .with_concepts_any(["traver"]),
        })
        .unwrap();

    assert_eq!(page.thoughts.len(), 1);
    assert_eq!(page.thoughts[0].content, "first indexed match");
    assert!(page.has_more);
    assert_eq!(
        page.next_cursor.as_ref().map(|cursor| cursor.index),
        Some(1)
    );
    assert_eq!(
        page.previous_cursor.as_ref().map(|cursor| cursor.index),
        Some(1)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_limit_one_supports_next_and_previous() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-single").unwrap();
    let thoughts = [
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "t0",
        ),
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Decision,
            ThoughtRole::Memory,
            "t1",
        ),
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Summary,
            ThoughtRole::Checkpoint,
            "t2",
        ),
    ];

    let next = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[0].id),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 1,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(next.thoughts.len(), 1);
    assert_eq!(next.thoughts[0].index, 1);

    let previous = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(thoughts[2].id),
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: false,
            chunk_size: 1,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(previous.thoughts.len(), 1);
    assert_eq!(previous.thoughts[0].index, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_from_genesis_and_head_anchors_work() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-boundaries").unwrap();

    let empty_forward = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert!(empty_forward.thoughts.is_empty());

    let empty_backward = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Head,
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert!(empty_backward.thoughts.is_empty());

    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "t0",
    );
    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "t1",
    );

    let from_genesis = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(from_genesis.thoughts[0].index, 0);

    let from_head = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Head,
            direction: ThoughtTraversalDirection::Backward,
            include_anchor: true,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert_eq!(from_head.thoughts[0].index, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_returns_empty_when_anchor_is_missing() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-missing-anchor").unwrap();
    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "t0",
    );

    let missing_id = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(Uuid::new_v4()),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert!(missing_id.thoughts.is_empty());

    let missing_hash = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Hash("missing-hash".to_string()),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 2,
            filter: ThoughtQuery::new(),
        })
        .unwrap();
    assert!(missing_hash.thoughts.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_handles_filters_that_match_nothing() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "traverse-no-match").unwrap();
    let anchor = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "t0",
    );
    append_test_thought(
        &mut chain,
        "apollo",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "t1",
    );

    let page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Id(anchor.id),
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: false,
            chunk_size: 2,
            filter: ThoughtQuery::new().with_agent_ids(["nobody"]),
        })
        .unwrap();
    assert!(page.thoughts.is_empty());
    assert!(!page.has_more);
    assert!(page.next_cursor.is_none());
    assert!(page.previous_cursor.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traverse_thoughts_respects_timestamp_window_helpers_for_seconds_and_milliseconds() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "time-window-helper").unwrap();

    let first = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Insight,
        ThoughtRole::Memory,
        "t0",
    );
    sleep(Duration::from_millis(5));
    let second = append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Decision,
        ThoughtRole::Checkpoint,
        "t1",
    );
    sleep(Duration::from_millis(5));
    append_test_thought(
        &mut chain,
        "astro",
        ThoughtType::Summary,
        ThoughtRole::Checkpoint,
        "t2",
    );

    let start_ms = first.timestamp.timestamp_millis();
    let delta_ms = (second.timestamp.timestamp_millis() - start_ms) as u64;
    let ms_page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 8,
            filter: ThoughtQuery::new()
                .with_time_window(ThoughtTimeWindow {
                    start: start_ms,
                    delta: delta_ms,
                    unit: TimeWindowUnit::Milliseconds,
                })
                .unwrap(),
        })
        .unwrap();

    let start_s = first.timestamp.timestamp();
    let delta_s = (second.timestamp.timestamp() - start_s) as u64;
    let second_page = chain
        .traverse_thoughts(&ThoughtTraversalRequest {
            anchor: ThoughtTraversalAnchor::Genesis,
            direction: ThoughtTraversalDirection::Forward,
            include_anchor: true,
            chunk_size: 8,
            filter: ThoughtQuery::new()
                .with_time_window(ThoughtTimeWindow {
                    start: start_s,
                    delta: delta_s,
                    unit: TimeWindowUnit::Seconds,
                })
                .unwrap(),
        })
        .unwrap();

    let ms_indexes: Vec<u64> = ms_page
        .thoughts
        .iter()
        .map(|thought| thought.index)
        .collect();
    let s_indexes: Vec<u64> = second_page
        .thoughts
        .iter()
        .map(|thought| thought.index)
        .collect();
    assert_eq!(ms_indexes, vec![0, 1]);
    assert!(s_indexes.starts_with(&ms_indexes));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hash_lookup_survives_reload() {
    let dir = unique_chain_dir();
    let thought = {
        let mut chain = MentisDb::open_with_key(&dir, "reload-hash").unwrap();
        append_test_thought(
            &mut chain,
            "astro",
            ThoughtType::Insight,
            ThoughtRole::Memory,
            "Persisted hash thought.",
        )
    };

    let reloaded = MentisDb::open_with_key(&dir, "reload-hash").unwrap();
    assert_eq!(
        reloaded.get_thought_by_id(thought.id).unwrap().hash,
        thought.hash
    );
    assert_eq!(
        reloaded.get_thought_by_hash(&thought.hash).unwrap().id,
        thought.id
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[derive(Clone)]
struct MemoryStorageAdapter {
    location: String,
    thoughts: Arc<Mutex<Vec<Thought>>>,
}

impl MemoryStorageAdapter {
    fn new(location: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            thoughts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl StorageAdapter for MemoryStorageAdapter {
    fn load_thoughts(&self) -> std::io::Result<Vec<Thought>> {
        Ok(self.thoughts.lock().unwrap().clone())
    }

    fn append_thought(&self, thought: &Thought) -> std::io::Result<()> {
        self.thoughts.lock().unwrap().push(thought.clone());
        Ok(())
    }

    fn storage_location(&self) -> String {
        self.location.clone()
    }

    fn storage_kind(&self) -> StorageAdapterKind {
        StorageAdapterKind::Binary
    }

    fn storage_path(&self) -> Option<&std::path::Path> {
        None
    }
}

#[test]
fn custom_storage_adapter_can_back_a_chain() {
    let adapter = MemoryStorageAdapter::new("memory://test");
    let mut chain = MentisDb::open_with_storage(Box::new(adapter.clone())).unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Checkpoint,
            "Adapter-backed thought persisted.",
        )
        .unwrap();
    assert_eq!(chain.storage_location(), "memory://test");

    let reloaded = MentisDb::open_with_storage(Box::new(adapter)).unwrap();
    assert_eq!(reloaded.thoughts().len(), 1);
    assert_eq!(
        reloaded.thoughts()[0].content,
        "Adapter-backed thought persisted."
    );
}

#[test]
fn binary_storage_adapter_persists_and_reloads() {
    let dir = unique_chain_dir();
    let adapter = BinaryStorageAdapter::for_chain_key(&dir, "binary-demo");
    let expected_path = dir.join(mentisdb::chain_storage_filename(
        "binary-demo",
        StorageAdapterKind::Binary,
    ));

    let mut chain = MentisDb::open_with_storage(Box::new(adapter.clone())).unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Checkpoint,
            "Persist this in the binary adapter.",
        )
        .unwrap();

    let reloaded = MentisDb::open_with_storage(Box::new(adapter)).unwrap();
    assert_eq!(reloaded.thoughts().len(), 1);
    assert_eq!(
        reloaded.thoughts()[0].content,
        "Persist this in the binary adapter."
    );
    assert_eq!(
        reloaded.storage_location(),
        expected_path.display().to_string()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shared_chain_queries_can_filter_by_agent_identity() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "shared-project").unwrap();

    chain
        .append_thought(
            "agent-alpha",
            ThoughtInput::new(ThoughtType::Insight, "Rate limiting is upstream.")
                .with_agent_name("Planner")
                .with_agent_owner("team-red"),
        )
        .unwrap();
    chain
        .append_thought(
            "agent-beta",
            ThoughtInput::new(ThoughtType::Decision, "Use backoff and retry windows.")
                .with_agent_name("Executor")
                .with_agent_owner("team-blue"),
        )
        .unwrap();

    let by_name = chain.query(&ThoughtQuery::new().with_agent_names(["Planner"]));
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].agent_id, "agent-alpha");

    let by_owner = chain.query(&ThoughtQuery::new().with_agent_owners(["team-blue"]));
    assert_eq!(by_owner.len(), 1);
    assert_eq!(
        chain
            .agent_registry()
            .agents
            .get("agent-beta")
            .unwrap()
            .display_name,
        "Executor"
    );

    let by_text = chain.query(&ThoughtQuery::new().with_text("team-red"));
    assert_eq!(by_text.len(), 1);
    assert_eq!(
        chain
            .agent_registry()
            .agents
            .get("agent-alpha")
            .unwrap()
            .display_name,
        "Planner"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_markdown_groups_thoughts_into_sections() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent1", "Analyst", Some("memory"), None).unwrap();

    chain
        .append(
            "agent1",
            ThoughtType::PreferenceUpdate,
            "User prefers short Markdown outputs.",
        )
        .unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Decision,
            "Use SQLite for local memory indexing.",
        )
        .unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Wonder,
            "Would concept embeddings improve retrieval quality?",
        )
        .unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Question,
            "Should embeddings be optional?",
        )
        .unwrap();
    chain
        .append_thought(
            "agent1",
            ThoughtInput::new(
                ThoughtType::LessonLearned,
                "When a fix takes multiple failed passes, store the final operating rule for the next agent.",
            )
            .with_role(ThoughtRole::Retrospective),
        )
        .unwrap();

    let markdown = chain.to_memory_markdown(None);
    assert!(markdown.contains("# MEMORY"));
    assert!(markdown.contains("## Identity"));
    assert!(markdown.contains("## Constraints And Decisions"));
    assert!(markdown.contains("## Corrections"));
    assert!(markdown.contains("## Open Threads"));
    assert!(markdown.contains("User prefers short Markdown outputs."));
    assert!(markdown.contains("Would concept embeddings improve retrieval quality?"));
    assert!(markdown.contains("role Retrospective"));
    assert!(markdown.contains("When a fix takes multiple failed passes"));
    assert!(markdown.contains("agent agent1"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn filename_depends_only_on_chain_key() {
    let first = chain_filename("agent1", "Analyst", Some("rust"), Some("friendly"));
    let second = chain_filename("agent1", "Different", Some("go"), Some("severe"));
    let third = chain_filename("agent2", "Analyst", Some("rust"), Some("friendly"));

    assert_eq!(first, second);
    assert_ne!(first, third);
}

#[test]
fn chain_key_can_be_recovered_from_storage_filename() {
    let filename = chain_storage_filename("borganism-brain", StorageAdapterKind::Binary);
    let recovered = chain_key_from_storage_filename(&filename).unwrap();

    assert_eq!(recovered, "borganism-brain");
    assert!(chain_key_from_storage_filename("not-a-thoughtchain-file.txt").is_none());
}

fn write_legacy_v0_chain(dir: &PathBuf, chain_key: &str, kind: StorageAdapterKind) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(chain_storage_filename(chain_key, kind));
    let legacy = LegacyThoughtV0Record {
        id: Uuid::new_v4(),
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "legacy-agent".to_string(),
        agent_name: "Legacy Agent".to_string(),
        agent_owner: Some("legacy-team".to_string()),
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "Legacy thought content".to_string(),
        confidence: Some(0.8),
        importance: 0.9,
        tags: vec!["legacy".to_string()],
        concepts: vec!["migration".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "legacy-hash".to_string(),
    };

    match kind {
        StorageAdapterKind::Jsonl => {
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string(&legacy).unwrap()),
            )
            .unwrap();
        }
        StorageAdapterKind::Binary => {
            let payload =
                bincode::serde::encode_to_vec(&legacy, bincode::config::standard()).unwrap();
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&payload);
            std::fs::write(&path, bytes).unwrap();
        }
    }
}

#[test]
fn signable_payload_is_stable_for_normalized_input() {
    let first = signable_thought_payload(
        "astro",
        &ThoughtInput::new(ThoughtType::Decision, "Persist the agent registry.")
            .with_importance(1.2)
            .with_tags(["ops", "ops", " "])
            .with_concepts(["registry", "Registry"]),
    );
    let second = signable_thought_payload(
        "astro",
        &ThoughtInput::new(ThoughtType::Decision, "Persist the agent registry.")
            .with_importance(1.0)
            .with_tags(["ops"])
            .with_concepts(["registry"]),
    );

    assert_eq!(first, second);
}

#[test]
fn agent_registry_admin_methods_manage_metadata_and_keys() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "registry-admin").unwrap();

    let created = chain
        .upsert_agent(
            "agent-admin",
            Some("Registry Admin"),
            Some("@gubatron"),
            Some("Admin test agent"),
            Some(AgentStatus::Active),
        )
        .unwrap();
    assert_eq!(created.display_name, "Registry Admin");
    assert_eq!(created.owner.as_deref(), Some("@gubatron"));
    assert_eq!(created.description.as_deref(), Some("Admin test agent"));

    let described = chain
        .set_agent_description("agent-admin", Some("Updated admin agent"))
        .unwrap();
    assert_eq!(
        described.description.as_deref(),
        Some("Updated admin agent")
    );

    let aliased = chain.add_agent_alias("agent-admin", "astro-admin").unwrap();
    assert!(aliased.aliases.iter().any(|alias| alias == "astro-admin"));

    let keyed = chain
        .add_agent_key(
            "agent-admin",
            "main-ed25519",
            PublicKeyAlgorithm::Ed25519,
            vec![1, 2, 3, 4],
        )
        .unwrap();
    assert_eq!(keyed.public_keys.len(), 1);
    assert_eq!(keyed.public_keys[0].algorithm, PublicKeyAlgorithm::Ed25519);

    let revoked = chain
        .revoke_agent_key("agent-admin", "main-ed25519")
        .unwrap();
    assert!(revoked.public_keys[0].revoked_at.is_some());

    let disabled = chain.disable_agent("agent-admin").unwrap();
    assert_eq!(disabled.status, AgentStatus::Revoked);

    let listed = chain.list_agent_registry();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].agent_id, "agent-admin");

    drop(chain);

    let reloaded = MentisDb::open_with_key(&dir, "registry-admin").unwrap();
    let record = reloaded.get_agent("agent-admin").unwrap();
    assert_eq!(record.description.as_deref(), Some("Updated admin agent"));
    assert!(record.aliases.iter().any(|alias| alias == "astro-admin"));
    assert_eq!(record.status, AgentStatus::Revoked);
    assert_eq!(record.public_keys.len(), 1);
    assert!(record.public_keys[0].revoked_at.is_some());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v0_jsonl_and_binary_chains_to_v1() {
    for kind in [StorageAdapterKind::Jsonl, StorageAdapterKind::Binary] {
        let dir = unique_chain_dir();
        let chain_key = format!("legacy-{kind}");
        write_legacy_v0_chain(&dir, &chain_key, kind);

        let reports = migrate_registered_chains(&dir, |_| {}).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].chain_key, chain_key);
        assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Binary);
        assert_eq!(reports[0].to_version, MENTISDB_CURRENT_VERSION);

        let registry = load_registered_chains(&dir).unwrap();
        let entry = registry.chains.get(&chain_key).unwrap();
        assert_eq!(entry.version, MENTISDB_CURRENT_VERSION);
        assert_eq!(entry.storage_adapter, StorageAdapterKind::Binary);
        assert_eq!(entry.thought_count, 1);

        let chain = MentisDb::open_with_key(&dir, &chain_key).unwrap();
        assert_eq!(chain.thoughts().len(), 1);
        assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
        assert!(chain.thoughts()[0].signing_key_id.is_none());
        let record = chain.agent_registry().agents.get("legacy-agent").unwrap();
        assert_eq!(record.display_name, "Legacy Agent");
        assert_eq!(record.owner.as_deref(), Some("legacy-team"));
        let active_path = dir.join(chain_storage_filename(
            &chain_key,
            StorageAdapterKind::Binary,
        ));
        assert!(active_path.exists());

        let archived = dir
            .join("migrations")
            .join(format!("v{}_to_v{}", 0, MENTISDB_CURRENT_VERSION))
            .join(chain_storage_filename(&chain_key, kind));
        assert!(archived.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn migrate_v0_chains_can_target_an_explicit_storage_adapter() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-jsonl-explicit";
    write_legacy_v0_chain(&dir, chain_key, StorageAdapterKind::Binary);

    let reports =
        migrate_registered_chains_with_adapter(&dir, StorageAdapterKind::Jsonl, |_| {}).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Jsonl);

    let registry = load_registered_chains(&dir).unwrap();
    let entry = registry.chains.get(chain_key).unwrap();
    assert_eq!(entry.storage_adapter, StorageAdapterKind::Jsonl);

    let active_path = dir.join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl));
    assert!(active_path.exists());
    let archived = dir
        .join("migrations")
        .join(format!("v{}_to_v{}", 0, MENTISDB_CURRENT_VERSION))
        .join(chain_storage_filename(
            chain_key,
            StorageAdapterKind::Binary,
        ));
    assert!(archived.exists());

    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, chain_key, StorageAdapterKind::Jsonl)
            .unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn current_version_jsonl_chain_is_reconciled_to_default_binary_storage() {
    let dir = unique_chain_dir();
    let chain_key = "current-jsonl";

    {
        let adapter = JsonlStorageAdapter::for_chain_key(&dir, chain_key);
        let mut chain = MentisDb::open_with_storage(Box::new(adapter)).unwrap();
        chain
            .append_thought(
                "legacy-agent",
                ThoughtInput::new(ThoughtType::Insight, "Current schema chain in JSONL.")
                    .with_agent_name("Legacy Agent")
                    .with_agent_owner("legacy-team"),
            )
            .unwrap();
    }

    let before = load_registered_chains(&dir).unwrap();
    let before_entry = before.chains.get(chain_key).unwrap();
    assert_eq!(before_entry.version, MENTISDB_CURRENT_VERSION);
    assert_eq!(before_entry.storage_adapter, StorageAdapterKind::Jsonl);

    let reports =
        migrate_registered_chains_with_adapter(&dir, StorageAdapterKind::Binary, |_| {}).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].chain_key, chain_key);
    assert_eq!(reports[0].from_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(reports[0].to_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(reports[0].source_storage_adapter, StorageAdapterKind::Jsonl);
    assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Binary);

    let after = load_registered_chains(&dir).unwrap();
    let after_entry = after.chains.get(chain_key).unwrap();
    assert_eq!(after_entry.storage_adapter, StorageAdapterKind::Binary);

    let active_binary = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    assert!(active_binary.exists());
    let archived_jsonl = dir
        .join("migrations")
        .join(format!(
            "v{}_to_v{}",
            MENTISDB_CURRENT_VERSION, MENTISDB_CURRENT_VERSION
        ))
        .join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl));
    assert!(archived_jsonl.exists());

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    let record = chain.agent_registry().agents.get("legacy-agent").unwrap();
    assert_eq!(record.display_name, "Legacy Agent");
    assert_eq!(record.owner.as_deref(), Some("legacy-team"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn legacy_registry_filename_is_upgraded_to_mentisdb_registry_name() {
    let dir = unique_chain_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let legacy_registry_path = dir.join("thoughtchain-registry.json");
    std::fs::write(&legacy_registry_path, r#"{"version":1,"chains":{}}"#).unwrap();

    let registry = load_registered_chains(&dir).unwrap();
    assert_eq!(registry.version, MENTISDB_CURRENT_VERSION);
    assert!(!legacy_registry_path.exists());
    assert!(dir.join("mentisdb-registry.json").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
