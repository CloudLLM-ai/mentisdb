use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use mentisdb::{
    chain_filename, chain_key_from_storage_filename, chain_storage_filename,
    load_registered_chains, migrate_chain_hash_algorithm, migrate_registered_chains,
    migrate_registered_chains_with_adapter, signable_thought_payload, AgentStatus,
    BinaryStorageAdapter, MentisDb, MentisDbMigrationEvent, PublicKeyAlgorithm, StorageAdapter,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtTimeWindow, ThoughtTraversalAnchor, ThoughtTraversalDirection,
    ThoughtTraversalRequest, ThoughtType, TimeWindowUnit, FLUSH_THRESHOLD,
    MENTISDB_CURRENT_VERSION,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mirrors the binary layout written by the 0.5.1 (schema-V1) daemon.
/// Field order must match the serialized Thought struct exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyThoughtV0Record {
    schema_version: u32,
    id: Uuid,
    index: u64,
    timestamp: chrono::DateTime<chrono::Utc>,
    session_id: Option<Uuid>,
    agent_id: String,
    signing_key_id: Option<String>,
    thought_signature: Option<Vec<u8>>,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: String,
    confidence: Option<f32>,
    importance: f32,
    tags: Vec<String>,
    concepts: Vec<String>,
    refs: Vec<u64>,
    relations: Vec<LegacyTestRelation>,
    prev_hash: String,
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyTestRelation {
    kind: ThoughtRelationKind,
    target_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyV2ThoughtRelation {
    kind: ThoughtRelationKind,
    target_id: Uuid,
    #[serde(default)]
    chain_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyV2ThoughtRecord {
    #[serde(default = "default_schema_v2")]
    schema_version: u32,
    id: Uuid,
    index: u64,
    timestamp: chrono::DateTime<chrono::Utc>,
    session_id: Option<Uuid>,
    agent_id: String,
    #[serde(default)]
    signing_key_id: Option<String>,
    #[serde(default)]
    thought_signature: Option<Vec<u8>>,
    thought_type: ThoughtType,
    role: ThoughtRole,
    content: String,
    confidence: Option<f32>,
    importance: f32,
    tags: Vec<String>,
    concepts: Vec<String>,
    refs: Vec<u64>,
    relations: Vec<LegacyV2ThoughtRelation>,
    prev_hash: String,
    hash: String,
}

fn default_schema_v2() -> u32 {
    2
}

fn write_mixed_v1_v2_binary_chain(dir: &PathBuf, chain_key: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    let first_id = Uuid::new_v4();
    let v1_thought = LegacyThoughtV0Record {
        schema_version: 1,
        id: first_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v1-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "V1 thought with empty relations".to_string(),
        confidence: Some(0.9),
        importance: 0.85,
        tags: vec!["v1".to_string()],
        concepts: vec!["migration".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "v1-hash-0".to_string(),
    };
    let second_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let v2_thought = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: second_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Decision,
        role: ThoughtRole::Memory,
        content: "V2 thought with relations".to_string(),
        confidence: Some(0.7),
        importance: 0.6,
        tags: vec!["v2".to_string()],
        concepts: vec!["migration".to_string()],
        refs: vec![0],
        relations: vec![
            LegacyV2ThoughtRelation {
                kind: ThoughtRelationKind::Supports,
                target_id: first_id,
                chain_key: None,
            },
            LegacyV2ThoughtRelation {
                kind: ThoughtRelationKind::References,
                target_id,
                chain_key: Some("other-chain".to_string()),
            },
        ],
        prev_hash: "v1-hash-0".to_string(),
        hash: "v2-hash-1".to_string(),
    };
    let mut bytes = Vec::new();
    let v1_payload =
        bincode::serde::encode_to_vec(&v1_thought, bincode::config::standard()).unwrap();
    bytes.extend_from_slice(&(v1_payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&v1_payload);
    let v2_payload =
        bincode::serde::encode_to_vec(&v2_thought, bincode::config::standard()).unwrap();
    bytes.extend_from_slice(&(v2_payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&v2_payload);
    std::fs::write(&path, bytes).unwrap();
}

fn write_legacy_v2_binary_chain(dir: &PathBuf, chain_key: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    let first_id = Uuid::new_v4();
    let first = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: first_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "V2 thought with relations".to_string(),
        confidence: Some(0.9),
        importance: 0.85,
        tags: vec!["v2".to_string()],
        concepts: vec!["migration".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "v2-hash-0".to_string(),
    };
    let second_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let second = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: second_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Decision,
        role: ThoughtRole::Memory,
        content: "V2 thought that references the first".to_string(),
        confidence: Some(0.7),
        importance: 0.6,
        tags: vec!["v2".to_string(), "decision".to_string()],
        concepts: vec!["migration".to_string(), "cross-chain".to_string()],
        refs: vec![0],
        relations: vec![
            LegacyV2ThoughtRelation {
                kind: ThoughtRelationKind::Supports,
                target_id: first_id,
                chain_key: None,
            },
            LegacyV2ThoughtRelation {
                kind: ThoughtRelationKind::References,
                target_id,
                chain_key: Some("other-chain".to_string()),
            },
        ],
        prev_hash: "v2-hash-0".to_string(),
        hash: "v2-hash-1".to_string(),
    };
    let mut bytes = Vec::new();
    for thought in [&first, &second] {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();
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
                chain_key: None,
                valid_at: None,
                invalid_at: None,
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

struct FailingStorageAdapter;
impl StorageAdapter for FailingStorageAdapter {
    fn load_thoughts(&self) -> std::io::Result<Vec<Thought>> {
        Ok(Vec::new())
    }
    fn append_thought(&self, _thought: &Thought) -> std::io::Result<()> {
        Err(std::io::Error::other("disk fell out"))
    }
    fn storage_location(&self) -> String {
        "failing://".to_string()
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
fn test_append_thought_propagates_adapter_error() {
    let adapter = FailingStorageAdapter;
    let mut chain = MentisDb::open_with_storage(Box::new(adapter)).unwrap();
    let result = chain.append("agent1", ThoughtType::Insight, "This should fail");
    assert!(result.is_err(), "append should propagate adapter error");
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(
        err.to_string().contains("disk fell out"),
        "expected 'disk fell out' error message"
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
fn mentisdb_buffered_auto_flush_false_still_persists_on_drop() {
    let dir = unique_chain_dir();
    let chain_key = "buffered-drop";

    {
        let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        chain.set_auto_flush(false).unwrap();
        chain
            .append(
                "agent1",
                ThoughtType::Checkpoint,
                "Persist this through the background writer.",
            )
            .unwrap();
    }

    let reloaded = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(reloaded.thoughts().len(), 1);
    assert_eq!(
        reloaded.thoughts()[0].content,
        "Persist this through the background writer."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mentisdb_switching_back_to_auto_flush_drains_background_writer() {
    let dir = unique_chain_dir();
    let chain_key = "buffered-switch";

    {
        let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        chain.set_auto_flush(false).unwrap();
        chain
            .append(
                "agent1",
                ThoughtType::Checkpoint,
                "Buffered before switching back.",
            )
            .unwrap();
        chain.set_auto_flush(true).unwrap();
        chain
            .append(
                "agent1",
                ThoughtType::Checkpoint,
                "Immediate after switching back.",
            )
            .unwrap();
    }

    let reloaded = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(reloaded.thoughts().len(), 2);
    assert_eq!(
        reloaded.thoughts()[0].content,
        "Buffered before switching back."
    );
    assert_eq!(
        reloaded.thoughts()[1].content,
        "Immediate after switching back."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mentisdb_auto_flush_true_persists_before_drop_via_group_commit() {
    let dir = unique_chain_dir();
    let chain_key = "durable-group-commit";

    let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    chain.set_auto_flush(true).unwrap();
    chain
        .append(
            "agent1",
            ThoughtType::Checkpoint,
            "Durably acknowledged before drop.",
        )
        .unwrap();

    let reloaded = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(reloaded.thoughts().len(), 1);
    assert_eq!(
        reloaded.thoughts()[0].content,
        "Durably acknowledged before drop."
    );

    drop(chain);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mentisdb_buffered_auto_flush_false_flushes_agent_registry_metadata_on_drop() {
    let dir = unique_chain_dir();
    let chain_key = "buffered-agent-registry";

    {
        let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        chain.set_auto_flush(false).unwrap();
        chain
            .append_thought(
                "agent1",
                ThoughtInput::new(
                    ThoughtType::Checkpoint,
                    "Persist agent metadata through buffered mode.",
                )
                .with_agent_name("Buffered Agent")
                .with_agent_owner("ops"),
            )
            .unwrap();
    }

    let reloaded = MentisDb::open_with_key(&dir, chain_key).unwrap();
    let agent = reloaded.get_agent("agent1").unwrap();
    assert_eq!(agent.display_name, "Buffered Agent");
    assert_eq!(agent.owner.as_deref(), Some("ops"));

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

fn write_legacy_v0_jsonl_chain(dir: &PathBuf, chain_key: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl));
    let legacy = LegacyThoughtV0Record {
        schema_version: 1,
        id: Uuid::new_v4(),
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "legacy-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
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
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy).unwrap()),
    )
    .unwrap();
}

fn write_legacy_v0_binary_chain(dir: &PathBuf, chain_key: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    let legacy = LegacyThoughtV0Record {
        schema_version: 1,
        id: Uuid::new_v4(),
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "legacy-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
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
    let payload = bincode::serde::encode_to_vec(&legacy, bincode::config::standard()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&payload);
    std::fs::write(&path, bytes).unwrap();
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

    chain
        .append_thought(
            "agent-admin",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Append without agent_name should preserve registered metadata.",
            ),
        )
        .unwrap();
    let after_append = chain.get_agent("agent-admin").unwrap();
    assert_eq!(after_append.display_name, "Registry Admin");
    assert_eq!(
        after_append.description.as_deref(),
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
fn migrate_v0_jsonl_chain_to_binary() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-jsonl";
    write_legacy_v0_jsonl_chain(&dir, chain_key);

    let reports = migrate_registered_chains(&dir, |_| {}).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].chain_key, chain_key);
    assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Binary);
    assert_eq!(reports[0].to_version, MENTISDB_CURRENT_VERSION);

    let registry = load_registered_chains(&dir).unwrap();
    let entry = registry.chains.get(chain_key).unwrap();
    assert_eq!(entry.version, MENTISDB_CURRENT_VERSION);
    assert_eq!(entry.storage_adapter, StorageAdapterKind::Binary);
    assert_eq!(entry.thought_count, 1);

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert!(chain.thoughts()[0].signing_key_id.is_none());
    let record = chain.agent_registry().agents.get("legacy-agent").unwrap();
    assert_eq!(record.display_name, "legacy-agent");
    let active_path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    assert!(active_path.exists());

    let archived = dir
        .join("migrations")
        .join(format!("v{}_to_v{}", 0, MENTISDB_CURRENT_VERSION))
        .join(chain_storage_filename(chain_key, StorageAdapterKind::Jsonl));
    assert!(archived.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v0_binary_chain_to_latest() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-binary";
    write_legacy_v0_binary_chain(&dir, chain_key);

    let reports = migrate_registered_chains(&dir, |_| {}).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].chain_key, chain_key);
    assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Binary);
    assert_eq!(reports[0].to_version, MENTISDB_CURRENT_VERSION);

    let registry = load_registered_chains(&dir).unwrap();
    let entry = registry.chains.get(chain_key).unwrap();
    assert_eq!(entry.version, MENTISDB_CURRENT_VERSION);
    assert_eq!(entry.storage_adapter, StorageAdapterKind::Binary);
    assert_eq!(entry.thought_count, 1);

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert!(chain.thoughts()[0].signing_key_id.is_none());
    let record = chain.agent_registry().agents.get("legacy-agent").unwrap();
    assert_eq!(record.display_name, "legacy-agent");
    let active_path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    assert!(active_path.exists());

    let archived = dir
        .join("migrations")
        .join(format!(
            "v{}_to_v{}",
            reports[0].from_version, MENTISDB_CURRENT_VERSION
        ))
        .join(chain_storage_filename(
            chain_key,
            StorageAdapterKind::Binary,
        ));
    assert!(archived.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v2_binary_chain_to_latest() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-v2";
    write_legacy_v2_binary_chain(&dir, chain_key);

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[0].thought_type, ThoughtType::Insight);
    assert_eq!(chain.thoughts()[1].thought_type, ThoughtType::Decision);
    assert_eq!(chain.thoughts()[0].content, "V2 thought with relations");
    assert_eq!(
        chain.thoughts()[1].content,
        "V2 thought that references the first"
    );
    assert!(chain.thoughts()[0].relations.is_empty());
    assert_eq!(chain.thoughts()[1].relations.len(), 2);
    assert_eq!(
        chain.thoughts()[1].relations[0].kind,
        ThoughtRelationKind::Supports
    );
    assert!(chain.thoughts()[1].relations[0].chain_key.is_none());
    assert!(chain.thoughts()[1].relations[0].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[0].invalid_at.is_none());
    assert_eq!(
        chain.thoughts()[1].relations[1].kind,
        ThoughtRelationKind::References
    );
    assert_eq!(
        chain.thoughts()[1].relations[1].chain_key,
        Some("other-chain".to_string())
    );
    assert!(chain.thoughts()[1].relations[1].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[1].invalid_at.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_mixed_v1_v2_binary_chain_to_latest() {
    let dir = unique_chain_dir();
    let chain_key = "mixed-v1-v2";
    write_mixed_v1_v2_binary_chain(&dir, chain_key);

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);
    assert!(chain.thoughts()[0].relations.is_empty());
    assert_eq!(chain.thoughts()[1].relations.len(), 2);
    assert_eq!(
        chain.thoughts()[1].relations[0].kind,
        ThoughtRelationKind::Supports
    );
    assert_eq!(
        chain.thoughts()[1].relations[1].kind,
        ThoughtRelationKind::References
    );
    assert_eq!(
        chain.thoughts()[1].relations[1].chain_key,
        Some("other-chain".to_string())
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v0_chains_with_explicit_binary_adapter() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-binary-explicit";
    write_legacy_v0_binary_chain(&dir, chain_key);

    let reports =
        migrate_registered_chains_with_adapter(&dir, StorageAdapterKind::Binary, |_| {}).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].storage_adapter, StorageAdapterKind::Binary);

    let registry = load_registered_chains(&dir).unwrap();
    let entry = registry.chains.get(chain_key).unwrap();
    assert_eq!(entry.storage_adapter, StorageAdapterKind::Binary);

    let archived = dir
        .join("migrations")
        .join(format!(
            "v{}_to_v{}",
            reports[0].from_version, MENTISDB_CURRENT_VERSION
        ))
        .join(chain_storage_filename(
            chain_key,
            StorageAdapterKind::Binary,
        ));

    let active_path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    assert!(active_path.exists());
    assert!(archived.exists());

    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, chain_key, StorageAdapterKind::Binary)
            .unwrap();
    assert_eq!(chain.thoughts().len(), 1);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn current_version_jsonl_chain_is_reconciled_to_default_binary_storage() {
    use mentisdb::{AgentRecord, AgentRegistry, MentisDbRegistration, MentisDbRegistry};
    use std::collections::BTreeMap;

    let dir = unique_chain_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let chain_key = "current-jsonl";

    // Write a raw .jsonl file with a current-schema Thought, plus the companion
    // agent-registry sidecar and mentisdb-registry.json, all declaring
    // storage_adapter=Jsonl. This simulates a chain that was written by an older
    // version of MentisDb that still used JSONL as its active format.
    {
        // Use a throwaway binary chain to produce a properly hashed Thought.
        let tmp_dir = dir.join("_tmp_setup");
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let mut tmp_chain = MentisDb::open_with_key(&tmp_dir, "tmp").unwrap();
        tmp_chain
            .append_thought(
                "legacy-agent",
                ThoughtInput::new(ThoughtType::Insight, "Current schema chain in JSONL.")
                    .with_agent_name("Legacy Agent")
                    .with_agent_owner("legacy-team"),
            )
            .unwrap();
        let thought = tmp_chain.thoughts()[0].clone();
        std::fs::remove_dir_all(&tmp_dir).unwrap();

        // Write the thought as raw JSONL.
        let jsonl_filename = chain_storage_filename(chain_key, StorageAdapterKind::Jsonl);
        let jsonl_path = dir.join(&jsonl_filename);
        std::fs::write(
            &jsonl_path,
            format!("{}\n", serde_json::to_string(&thought).unwrap()),
        )
        .unwrap();

        // Write the agent-registry sidecar (.agents.json) for the jsonl chain.
        let stem = jsonl_filename
            .strip_suffix(".jsonl")
            .unwrap_or(&jsonl_filename);
        let agents_path = dir.join(format!("{stem}.agents.json"));
        let now = chrono::Utc::now();
        let mut agent_map: BTreeMap<String, AgentRecord> = BTreeMap::new();
        agent_map.insert(
            "legacy-agent".to_string(),
            AgentRecord {
                agent_id: "legacy-agent".to_string(),
                display_name: "Legacy Agent".to_string(),
                owner: Some("legacy-team".to_string()),
                description: None,
                aliases: vec![],
                public_keys: vec![],
                status: mentisdb::AgentStatus::Active,
                first_seen_index: Some(0),
                last_seen_index: Some(0),
                first_seen_at: Some(now),
                last_seen_at: Some(now),
                thought_count: 1,
            },
        );
        let agent_registry = AgentRegistry { agents: agent_map };
        std::fs::write(
            &agents_path,
            serde_json::to_string_pretty(&agent_registry).unwrap(),
        )
        .unwrap();

        // Write mentisdb-registry.json declaring this chain as Jsonl at current version.
        let storage_location = jsonl_path.display().to_string();
        let registry = MentisDbRegistry {
            version: 1,
            chains: {
                let mut map = BTreeMap::new();
                map.insert(
                    chain_key.to_string(),
                    MentisDbRegistration {
                        chain_key: chain_key.to_string(),
                        version: MENTISDB_CURRENT_VERSION,
                        storage_adapter: StorageAdapterKind::Jsonl,
                        storage_location,
                        thought_count: 1,
                        agent_count: 1,
                        created_at: now,
                        updated_at: now,
                    },
                );
                map
            },
        };
        let registry_path = dir.join("mentisdb-registry.json");
        std::fs::write(
            &registry_path,
            serde_json::to_string_pretty(&registry).unwrap(),
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
fn upsert_agent_accepts_case_only_display_name_updates_without_alias_churn() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "case-only-display-name").unwrap();

    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Seed stub record from a thought."),
        )
        .unwrap();

    let updated = chain
        .upsert_agent("astro", Some("Astro"), None, None, None)
        .unwrap();
    assert_eq!(updated.display_name, "Astro");
    assert!(updated.aliases.is_empty());

    let persisted = chain.get_agent("astro").unwrap();
    assert_eq!(persisted.display_name, "Astro");
    assert!(persisted.aliases.is_empty());

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

// ── v0.5.2 tests: Reframe, Supersedes, cross-chain ThoughtRelation ────────────

/// Verifies that a `Reframe` thought can be appended and read back with the
/// correct `ThoughtType`.
#[test]
fn test_reframe_thought_type_roundtrip() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "reframe-roundtrip").unwrap();

    let thought = chain
        .append(
            "agent-reframe",
            ThoughtType::Reframe,
            "The failure was not a disaster but a learning opportunity.",
        )
        .unwrap();

    assert_eq!(thought.thought_type, ThoughtType::Reframe);

    // Reload from disk and verify persistence
    let reloaded = MentisDb::open_with_key(&dir, "reframe-roundtrip").unwrap();
    assert_eq!(reloaded.thoughts().len(), 1);
    assert_eq!(reloaded.thoughts()[0].thought_type, ThoughtType::Reframe);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Verifies that a `Reframe` thought can reference an earlier thought via a
/// `Supersedes` relation, and that the relation is stored and retrieved correctly.
#[test]
fn test_supersedes_relation() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "supersedes-relation").unwrap();

    let first = chain
        .append(
            "agent1",
            ThoughtType::FactLearned,
            "We must never retry on timeout.",
        )
        .unwrap();
    let first_id = first.id;

    let input = ThoughtInput::new(
        ThoughtType::Reframe,
        "Retrying on timeout is fine with exponential backoff; the prior rule was too broad.",
    )
    .with_relations(vec![ThoughtRelation {
        kind: ThoughtRelationKind::Supersedes,
        target_id: first_id,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }]);

    let second = chain.append_thought("agent1", input).unwrap();

    assert_eq!(second.relations.len(), 1);
    assert_eq!(second.relations[0].kind, ThoughtRelationKind::Supersedes);
    assert_eq!(second.relations[0].target_id, first_id);
    assert!(second.relations[0].chain_key.is_none());

    // Reload from disk and verify
    let reloaded = MentisDb::open_with_key(&dir, "supersedes-relation").unwrap();
    let reloaded_second = &reloaded.thoughts()[1];
    assert_eq!(
        reloaded_second.relations[0].kind,
        ThoughtRelationKind::Supersedes
    );
    assert_eq!(reloaded_second.relations[0].target_id, first_id);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Verifies that a cross-chain `ThoughtRelation` (with `chain_key: Some(...)`)
/// survives a JSON serialize → deserialize roundtrip with `chain_key` preserved.
#[test]
fn test_cross_chain_relation_serde() {
    let target = Uuid::new_v4();
    let relation = ThoughtRelation {
        kind: ThoughtRelationKind::Supersedes,
        target_id: target,
        chain_key: Some("other-chain".to_string()),
        valid_at: None,
        invalid_at: None,
    };

    let json = serde_json::to_string(&relation).unwrap();
    let deserialized: ThoughtRelation = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.kind, ThoughtRelationKind::Supersedes);
    assert_eq!(deserialized.target_id, target);
    assert_eq!(deserialized.chain_key.as_deref(), Some("other-chain"));
}

/// Verifies that an intra-chain `ThoughtRelation` (with `chain_key: None`)
/// serializes to JSON WITHOUT a `chain_key` field (backward-compatible),
/// and deserializes back with `chain_key == None`.
#[test]
fn test_intra_chain_relation_backward_compat() {
    let target = Uuid::new_v4();
    let relation = ThoughtRelation {
        kind: ThoughtRelationKind::References,
        target_id: target,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    };

    let json = serde_json::to_string(&relation).unwrap();

    // The field must be absent when None (skip_serializing_if = "Option::is_none")
    assert!(
        !json.contains("chain_key"),
        "Expected no 'chain_key' field in JSON for intra-chain relation, got: {json}"
    );

    let deserialized: ThoughtRelation = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.chain_key, None);
    assert_eq!(deserialized.target_id, target);
}

// ---------------------------------------------------------------------------
// import_from_memory_markdown tests
// ---------------------------------------------------------------------------

/// Verifies the basic round-trip: export a chain with several thought types,
/// import the markdown into a new chain, and confirm that the imported thought
/// count and types match the originals.
#[test]
fn test_import_memory_markdown_basic() {
    let dir = unique_chain_dir();

    // Build a source chain with a mix of types.
    let mut src = MentisDb::open_with_key(&dir, "src-chain").unwrap();
    src.append_thought(
        "alice",
        ThoughtInput::new(ThoughtType::Decision, "Use PostgreSQL")
            .with_importance(0.90)
            .with_confidence(0.95),
    )
    .unwrap();
    src.append_thought(
        "alice",
        ThoughtInput::new(ThoughtType::Insight, "Connection pooling matters").with_importance(0.75),
    )
    .unwrap();
    src.append_thought(
        "bob",
        ThoughtInput::new(ThoughtType::Correction, "Timeout is 30s not 10s")
            .with_role(ThoughtRole::Retrospective)
            .with_importance(0.80),
    )
    .unwrap();

    let markdown = src.to_memory_markdown(None);

    // Import into a fresh destination chain.
    let dst_dir = unique_chain_dir();
    let mut dst = MentisDb::open_with_key(&dst_dir, "dst-chain").unwrap();
    let indices = dst
        .import_from_memory_markdown(&markdown, "fallback-agent")
        .unwrap();

    assert_eq!(
        indices.len(),
        3,
        "expected 3 imported thoughts, got {}: markdown=\n{markdown}",
        indices.len()
    );

    // Verify all imported indices are sequential starting at 0.
    assert_eq!(indices, vec![0, 1, 2]);

    // to_memory_markdown groups thoughts by section (Knowledge before
    // Constraints And Decisions before Corrections), so the imported order
    // is Insight → Decision → Correction, not the original append order.
    let thoughts = dst.thoughts();
    let types: Vec<ThoughtType> = thoughts.iter().map(|t| t.thought_type).collect();
    assert!(
        types.contains(&ThoughtType::Decision),
        "imported chain should contain a Decision thought"
    );
    assert!(
        types.contains(&ThoughtType::Insight),
        "imported chain should contain an Insight thought"
    );
    assert!(
        types.contains(&ThoughtType::Correction),
        "imported chain should contain a Correction thought"
    );

    // Verify agent IDs were captured from metadata.
    // Decision and Insight → alice; Correction → bob.
    for t in thoughts {
        match t.thought_type {
            ThoughtType::Decision | ThoughtType::Insight => {
                assert_eq!(
                    t.agent_id, "alice",
                    "expected alice for {:?}",
                    t.thought_type
                );
            }
            ThoughtType::Correction => {
                assert_eq!(t.agent_id, "bob", "expected bob for Correction");
            }
            _ => {}
        }
    }
}

/// Verifies that when no `agent` token is present in the metadata the
/// `default_agent_id` parameter is used for the imported thought.
#[test]
fn test_import_memory_markdown_default_agent() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "default-agent-chain").unwrap();

    // Markdown with no agent token — stripped metadata for the first line,
    // and a second line that has an explicit agent to confirm both code-paths.
    let markdown = "## Decisions\n\
        - [#0] Decision: No-agent line (importance 0.70)\n\
        - [#1] Insight: Has agent line (agent explicit-agent; importance 0.60)\n";

    let indices = chain
        .import_from_memory_markdown(markdown, "my-default")
        .unwrap();

    assert_eq!(indices.len(), 2, "expected 2 imported thoughts");

    let thoughts = chain.thoughts();
    assert_eq!(
        thoughts[0].agent_id, "my-default",
        "no-agent line should fall back to default_agent_id"
    );
    assert_eq!(
        thoughts[1].agent_id, "explicit-agent",
        "explicit agent in metadata should override default"
    );
}

/// Verifies that malformed or non-matching lines are silently skipped while
/// valid lines are still imported correctly.
#[test]
fn test_import_memory_markdown_partial() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "partial-chain").unwrap();

    // Mix of: valid lines, section header, blank line, malformed bullet,
    // a line with an unknown thought type.
    let markdown = "\
        # MEMORY\n\
        \n\
        ## Decisions\n\
        \n\
        - [#0] Decision: Valid decision (agent alice; importance 0.85)\n\
        - this line has no index bracket\n\
        - [#2] UnknownType: Unknown type should be skipped (agent bob; importance 0.50)\n\
        - [#3] Insight: Second valid insight (agent carol; importance 0.70)\n\
        Just a plain prose paragraph.\n\
    ";

    let indices = chain
        .import_from_memory_markdown(markdown, "fallback")
        .unwrap();

    assert_eq!(
        indices.len(),
        2,
        "expected 2 valid thoughts imported, got {}",
        indices.len()
    );

    let thoughts = chain.thoughts();
    assert_eq!(thoughts[0].thought_type, ThoughtType::Decision);
    assert_eq!(thoughts[0].agent_id, "alice");
    assert_eq!(thoughts[1].thought_type, ThoughtType::Insight);
    assert_eq!(thoughts[1].agent_id, "carol");
}

// ---------------------------------------------------------------------------
// Hash algorithm migration tests (0.7.7 JSON hashes → 0.7.8 bincode hashes)
// ---------------------------------------------------------------------------

/// Recomputes thought hashes using the legacy JSON algorithm and patches both
/// `hash` and `prev_hash` fields in the slice, then rewrites the binary file.
/// Used only to simulate a 0.7.7 chain file for migration tests.
fn patch_chain_file_to_legacy_hashes(path: &std::path::Path, thoughts: &mut [Thought]) {
    use sha2::{Digest, Sha256};

    // CanonicalThought must match the field order in compute_thought_hash_legacy exactly.
    // JSON field order follows struct declaration order — any mismatch produces wrong hashes.
    #[derive(serde::Serialize)]
    struct CanonicalThought<'a> {
        schema_version: u32,
        id: uuid::Uuid,
        index: u64,
        timestamp: &'a chrono::DateTime<chrono::Utc>,
        session_id: Option<uuid::Uuid>,
        agent_id: &'a str,
        signing_key_id: Option<&'a str>,
        thought_signature: Option<&'a [u8]>,
        thought_type: ThoughtType,
        role: ThoughtRole,
        content: &'a str,
        confidence: Option<f32>,
        importance: f32,
        tags: &'a [String],
        concepts: &'a [String],
        refs: &'a [u64],
        relations: &'a [ThoughtRelation],
        prev_hash: &'a str,
    }

    let json_hash = |t: &Thought| -> String {
        let canonical = CanonicalThought {
            schema_version: t.schema_version,
            id: t.id,
            index: t.index,
            timestamp: &t.timestamp,
            session_id: t.session_id,
            agent_id: &t.agent_id,
            signing_key_id: t.signing_key_id.as_deref(),
            thought_signature: t.thought_signature.as_deref(),
            thought_type: t.thought_type,
            role: t.role,
            content: &t.content,
            confidence: t.confidence,
            importance: t.importance,
            tags: &t.tags,
            concepts: &t.concepts,
            refs: &t.refs,
            relations: &t.relations,
            prev_hash: &t.prev_hash,
        };
        let bytes = serde_json::to_vec(&canonical).expect("json serialization");
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    };

    let mut prev = String::new();
    for thought in thoughts.iter_mut() {
        thought.prev_hash = prev.clone();
        thought.hash = json_hash(thought);
        prev = thought.hash.clone();
    }

    // Rewrite the binary file.
    if path.exists() {
        std::fs::remove_file(path).unwrap();
    }
    for thought in thoughts.iter() {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        use std::io::Write;
        file.write_all(&(payload.len() as u64).to_le_bytes())
            .unwrap();
        file.write_all(&payload).unwrap();
    }
}

#[test]
fn test_hash_rehash_migration_v077_to_v078() {
    let dir = unique_chain_dir();
    let chain_key = "legacy-chain";

    // 1. Write a chain — file will have current bincode hashes.
    {
        let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        chain
            .append("agent1", ThoughtType::Decision, "Use Redis for caching")
            .unwrap();
        chain
            .append(
                "agent1",
                ThoughtType::Insight,
                "Redis fits our latency needs",
            )
            .unwrap();
    }

    // 2. Load the thoughts and patch hashes back to legacy JSON algorithm,
    //    simulating a chain file written by mentisdb ≤ 0.7.7.
    let chain_path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));
    {
        let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
        let mut thoughts: Vec<Thought> = chain.thoughts().to_vec();
        patch_chain_file_to_legacy_hashes(&chain_path, &mut thoughts);
    }

    // 3. Run the migration — it detects the legacy-hashed file and rehashes it.
    let mut started: Vec<String> = vec![];
    let mut completed: Vec<String> = vec![];
    let count = migrate_chain_hash_algorithm(&dir, |event| match event {
        MentisDbMigrationEvent::StartedHashRehash { chain_key, .. } => {
            started.push(chain_key);
        }
        MentisDbMigrationEvent::CompletedHashRehash { chain_key, .. } => {
            completed.push(chain_key);
        }
        _ => {}
    })
    .unwrap();

    assert_eq!(count, 1, "exactly one chain should have been rehashed");
    assert_eq!(started, vec![chain_key]);
    assert_eq!(completed, vec![chain_key]);

    // 5. After migration the chain opens and passes integrity with bincode hashes.
    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert!(
        chain.verify_integrity(),
        "chain must pass bincode integrity after migration"
    );

    // 6. A second migration run is a no-op (no legacy hashes remain).
    let count2 = migrate_chain_hash_algorithm(&dir, |_| {}).unwrap();
    assert_eq!(count2, 0, "second migration run must be a no-op");
}

// ── v0.8.2 tests: Memory dedup/merge ────────────────────────────────────────

#[test]
fn dedup_threshold_disabled_by_default() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain
        .append("agent", ThoughtType::FactLearned, "The sky is blue.")
        .unwrap();
    chain
        .append("agent", ThoughtType::FactLearned, "The sky is blue.")
        .unwrap();
    let thoughts = chain.thoughts();
    assert_eq!(thoughts.len(), 2);
    assert!(
        thoughts[1].relations.is_empty(),
        "no Supersedes relation when dedup is disabled"
    );
}

#[test]
fn dedup_auto_supersedes_similar_content() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain.with_dedup_threshold(Some(0.85));
    chain
        .append(
            "agent",
            ThoughtType::FactLearned,
            "The sky is blue on clear days.",
        )
        .unwrap();
    chain
        .append(
            "agent",
            ThoughtType::FactLearned,
            "The sky is blue on clear days.",
        )
        .unwrap();
    let thoughts = chain.thoughts();
    assert_eq!(thoughts.len(), 2);
    assert_eq!(thoughts[1].relations.len(), 1);
    assert_eq!(
        thoughts[1].relations[0].kind,
        ThoughtRelationKind::Supersedes
    );
    assert_eq!(thoughts[1].relations[0].target_id, thoughts[0].id);
}

#[test]
fn dedup_no_supersedes_for_dissimilar_content() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain.with_dedup_threshold(Some(0.85));
    chain
        .append("agent", ThoughtType::FactLearned, "The sky is blue.")
        .unwrap();
    chain
        .append(
            "agent",
            ThoughtType::FactLearned,
            "Water boils at 100 degrees Celsius.",
        )
        .unwrap();
    let thoughts = chain.thoughts();
    assert_eq!(thoughts.len(), 2);
    assert!(
        thoughts[1].relations.is_empty(),
        "no Supersedes for dissimilar content"
    );
}

#[test]
fn dedup_invalidated_set_updated_on_auto_supersedes() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain.with_dedup_threshold(Some(0.85));
    chain
        .append("agent", ThoughtType::FactLearned, "The sky is blue.")
        .unwrap();
    let first_id = chain.thoughts()[0].id;
    chain
        .append("agent", ThoughtType::FactLearned, "The sky is blue.")
        .unwrap();
    assert!(
        chain.invalidated_thought_ids().contains(&first_id),
        "first thought should be in invalidated set"
    );
}

#[test]
fn dedup_scan_window_limits_comparison_range() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain.with_dedup_threshold(Some(0.85));
    chain.with_dedup_scan_window(2);
    for i in 0..5 {
        chain
            .append(
                "agent",
                ThoughtType::FactLearned,
                &format!("Fact number {i}."),
            )
            .unwrap();
    }
    chain
        .append("agent", ThoughtType::FactLearned, "Fact number 0.")
        .unwrap();
    let thoughts = chain.thoughts();
    let last = thoughts.last().unwrap();
    assert!(
        last.relations.is_empty(),
        "dedup should not find the very first thought outside the scan window"
    );
}

// ── v0.8.2 tests: Multi-level memory scopes ─────────────────────────────────

#[test]
fn scope_tag_added_on_append() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::FactLearned, "Session data")
                .with_scope(mentisdb::MemoryScope::Session),
        )
        .unwrap();
    let thought = &chain.thoughts()[0];
    assert!(
        thought.tags.contains(&"scope:session".to_string()),
        "scope:session tag should be present"
    );
}

#[test]
fn scope_filter_in_ranked_search() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::FactLearned, "User-wide fact")
                .with_scope(mentisdb::MemoryScope::User),
        )
        .unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::FactLearned, "Session fact")
                .with_scope(mentisdb::MemoryScope::Session),
        )
        .unwrap();
    let query = mentisdb::RankedSearchQuery::new()
        .with_text("fact")
        .with_scope(mentisdb::MemoryScope::Session);
    let result = chain.query_ranked(&query);
    assert_eq!(
        result.hits.len(),
        1,
        "only session-scoped thought should match"
    );
    assert!(
        result.hits[0]
            .thought
            .tags
            .contains(&"scope:session".to_string()),
        "matched thought should have scope:session tag"
    );
}

#[test]
fn scope_default_is_user() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open(&dir, "agent", "Agent", None, None).unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::FactLearned, "Default scope"),
        )
        .unwrap();
    let thought = &chain.thoughts()[0];
    assert!(
        !thought.tags.iter().any(|t| t.starts_with("scope:")),
        "default append should not add a scope tag"
    );
}

#[test]
fn scope_from_tag_roundtrip() {
    use mentisdb::MemoryScope;
    assert_eq!(MemoryScope::from_tag("scope:user"), Some(MemoryScope::User));
    assert_eq!(
        MemoryScope::from_tag("scope:session"),
        Some(MemoryScope::Session)
    );
    assert_eq!(
        MemoryScope::from_tag("scope:agent"),
        Some(MemoryScope::Agent)
    );
    assert_eq!(MemoryScope::from_tag("other"), None);
}

fn all_relation_kinds() -> Vec<ThoughtRelationKind> {
    vec![
        ThoughtRelationKind::References,
        ThoughtRelationKind::Summarizes,
        ThoughtRelationKind::Corrects,
        ThoughtRelationKind::Invalidates,
        ThoughtRelationKind::CausedBy,
        ThoughtRelationKind::Supports,
        ThoughtRelationKind::Contradicts,
        ThoughtRelationKind::DerivedFrom,
        ThoughtRelationKind::ContinuesFrom,
        ThoughtRelationKind::RelatedTo,
        ThoughtRelationKind::Supersedes,
    ]
}

fn all_thought_types() -> Vec<ThoughtType> {
    vec![
        ThoughtType::PreferenceUpdate,
        ThoughtType::UserTrait,
        ThoughtType::RelationshipUpdate,
        ThoughtType::Finding,
        ThoughtType::Insight,
        ThoughtType::FactLearned,
        ThoughtType::PatternDetected,
        ThoughtType::Hypothesis,
        ThoughtType::Mistake,
        ThoughtType::Correction,
        ThoughtType::LessonLearned,
        ThoughtType::AssumptionInvalidated,
        ThoughtType::Constraint,
        ThoughtType::Plan,
        ThoughtType::Subgoal,
        ThoughtType::Decision,
        ThoughtType::StrategyShift,
        ThoughtType::Wonder,
        ThoughtType::Question,
        ThoughtType::Idea,
        ThoughtType::Experiment,
        ThoughtType::ActionTaken,
        ThoughtType::TaskComplete,
        ThoughtType::Checkpoint,
        ThoughtType::StateSnapshot,
        ThoughtType::Handoff,
        ThoughtType::Summary,
        ThoughtType::Surprise,
        ThoughtType::Reframe,
        ThoughtType::Goal,
    ]
}

fn all_thought_roles() -> Vec<ThoughtRole> {
    vec![
        ThoughtRole::Memory,
        ThoughtRole::WorkingMemory,
        ThoughtRole::Summary,
        ThoughtRole::Compression,
        ThoughtRole::Checkpoint,
        ThoughtRole::Handoff,
        ThoughtRole::Audit,
        ThoughtRole::Retrospective,
    ]
}

#[test]
fn migrate_v2_chain_with_all_relation_kinds() {
    let dir = unique_chain_dir();
    let chain_key = "v2-all-relation-kinds";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let first_id = Uuid::new_v4();
    let first = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: first_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-rel-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "V2 anchor thought".to_string(),
        confidence: Some(0.9),
        importance: 0.85,
        tags: vec!["v2".to_string()],
        concepts: vec!["relations".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "v2-rel-hash-0".to_string(),
    };

    let kinds = all_relation_kinds();
    let second_id = Uuid::new_v4();
    let intra_relations: Vec<LegacyV2ThoughtRelation> = kinds
        .iter()
        .enumerate()
        .map(|(i, kind)| LegacyV2ThoughtRelation {
            kind: *kind,
            target_id: first_id,
            chain_key: if i % 2 == 0 {
                None
            } else {
                Some(format!("cross-chain-{}", i))
            },
        })
        .collect();
    let second = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: second_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-rel-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Decision,
        role: ThoughtRole::Memory,
        content: "V2 thought with all relation kinds".to_string(),
        confidence: Some(0.7),
        importance: 0.6,
        tags: vec!["v2".to_string(), "all-kinds".to_string()],
        concepts: vec!["relations".to_string()],
        refs: vec![0],
        relations: intra_relations,
        prev_hash: "v2-rel-hash-0".to_string(),
        hash: "v2-rel-hash-1".to_string(),
    };

    let mut bytes = Vec::new();
    for thought in [&first, &second] {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);
    assert!(chain.thoughts()[0].relations.is_empty());
    assert_eq!(chain.thoughts()[1].relations.len(), kinds.len());

    for (i, expected_kind) in kinds.iter().enumerate() {
        let rel = &chain.thoughts()[1].relations[i];
        assert_eq!(rel.kind, *expected_kind);
        assert_eq!(rel.target_id, first_id);
        if i % 2 == 0 {
            assert!(rel.chain_key.is_none());
        } else {
            assert_eq!(rel.chain_key, Some(format!("cross-chain-{}", i)));
        }
        assert!(rel.valid_at.is_none());
        assert!(rel.invalid_at.is_none());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v1_chain_with_all_thought_types() {
    let dir = unique_chain_dir();
    let chain_key = "v1-all-thought-types";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let types = all_thought_types();
    let mut bytes = Vec::new();
    for (i, thought_type) in types.iter().enumerate() {
        let record = LegacyThoughtV0Record {
            schema_version: 1,
            id: Uuid::new_v4(),
            index: i as u64,
            timestamp: chrono::Utc::now(),
            session_id: None,
            agent_id: "v1-type-agent".to_string(),
            signing_key_id: None,
            thought_signature: None,
            thought_type: *thought_type,
            role: ThoughtRole::Memory,
            content: format!("V1 thought type {:?}", thought_type),
            confidence: Some(0.8),
            importance: 0.7,
            tags: vec![format!("type-{}", i)],
            concepts: vec!["thought-types".to_string()],
            refs: vec![],
            relations: vec![],
            prev_hash: String::new(),
            hash: format!("v1-type-hash-{}", i),
        };
        let payload = bincode::serde::encode_to_vec(&record, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), types.len());
    for (i, expected_type) in types.iter().enumerate() {
        let thought = &chain.thoughts()[i];
        assert_eq!(thought.schema_version, MENTISDB_CURRENT_VERSION);
        assert_eq!(thought.thought_type, *expected_type);
        assert_eq!(
            thought.content,
            format!("V1 thought type {:?}", expected_type)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v2_chain_with_all_thought_roles() {
    let dir = unique_chain_dir();
    let chain_key = "v2-all-thought-roles";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let roles = all_thought_roles();
    let mut bytes = Vec::new();
    for (i, role) in roles.iter().enumerate() {
        let record = LegacyV2ThoughtRecord {
            schema_version: 2,
            id: Uuid::new_v4(),
            index: i as u64,
            timestamp: chrono::Utc::now(),
            session_id: None,
            agent_id: "v2-role-agent".to_string(),
            signing_key_id: None,
            thought_signature: None,
            thought_type: ThoughtType::Insight,
            role: *role,
            content: format!("V2 thought role {:?}", role),
            confidence: Some(0.8),
            importance: 0.7,
            tags: vec![format!("role-{}", i)],
            concepts: vec!["thought-roles".to_string()],
            refs: vec![],
            relations: vec![],
            prev_hash: String::new(),
            hash: format!("v2-role-hash-{}", i),
        };
        let payload = bincode::serde::encode_to_vec(&record, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), roles.len());
    for (i, expected_role) in roles.iter().enumerate() {
        let thought = &chain.thoughts()[i];
        assert_eq!(thought.schema_version, MENTISDB_CURRENT_VERSION);
        assert_eq!(thought.role, *expected_role);
        assert_eq!(
            thought.content,
            format!("V2 thought role {:?}", expected_role)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_mixed_chain_with_v1_v2_v3_thoughts() {
    let dir = unique_chain_dir();
    let chain_key = "mixed-v1-v2-v3";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let v1_id = Uuid::new_v4();
    let v1_thought = LegacyThoughtV0Record {
        schema_version: 1,
        id: v1_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "mixed-v1-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "V1 thought in mixed chain".to_string(),
        confidence: Some(0.9),
        importance: 0.85,
        tags: vec!["v1".to_string()],
        concepts: vec!["mixed".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "mixed-v1-hash-0".to_string(),
    };

    let v2_id = Uuid::new_v4();
    let v2_thought = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: v2_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "mixed-v2-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Decision,
        role: ThoughtRole::Memory,
        content: "V2 thought in mixed chain".to_string(),
        confidence: Some(0.7),
        importance: 0.6,
        tags: vec!["v2".to_string()],
        concepts: vec!["mixed".to_string()],
        refs: vec![0],
        relations: vec![LegacyV2ThoughtRelation {
            kind: ThoughtRelationKind::Supports,
            target_id: v1_id,
            chain_key: None,
        }],
        prev_hash: "mixed-v1-hash-0".to_string(),
        hash: "mixed-v2-hash-1".to_string(),
    };

    let mut bytes = Vec::new();
    let v1_payload =
        bincode::serde::encode_to_vec(&v1_thought, bincode::config::standard()).unwrap();
    bytes.extend_from_slice(&(v1_payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&v1_payload);
    let v2_payload =
        bincode::serde::encode_to_vec(&v2_thought, bincode::config::standard()).unwrap();
    bytes.extend_from_slice(&(v2_payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&v2_payload);
    std::fs::write(&path, &bytes).unwrap();

    let mut chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);

    let v3_thought = chain
        .append(
            "mixed-v3-agent",
            ThoughtType::FactLearned,
            "V3 thought already at current version",
        )
        .unwrap();
    assert_eq!(v3_thought.schema_version, MENTISDB_CURRENT_VERSION);

    drop(chain);

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 3);
    for thought in chain.thoughts() {
        assert_eq!(thought.schema_version, MENTISDB_CURRENT_VERSION);
    }
    assert_eq!(chain.thoughts()[0].content, "V1 thought in mixed chain");
    assert_eq!(chain.thoughts()[1].content, "V2 thought in mixed chain");
    assert_eq!(
        chain.thoughts()[2].content,
        "V3 thought already at current version"
    );
    assert_eq!(chain.thoughts()[1].relations.len(), 1);
    assert_eq!(
        chain.thoughts()[1].relations[0].kind,
        ThoughtRelationKind::Supports
    );
    assert!(chain.thoughts()[1].relations[0].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[0].invalid_at.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v2_chain_with_signed_thought() {
    let dir = unique_chain_dir();
    let chain_key = "v2-signed-thought";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let signed_id = Uuid::new_v4();
    let signed = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: signed_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-signed-agent".to_string(),
        signing_key_id: Some("key-ed25519-001".to_string()),
        thought_signature: Some(vec![1, 2, 3]),
        thought_type: ThoughtType::Decision,
        role: ThoughtRole::Memory,
        content: "V2 signed thought".to_string(),
        confidence: Some(0.95),
        importance: 0.9,
        tags: vec!["signed".to_string()],
        concepts: vec!["signing".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "v2-signed-hash-0".to_string(),
    };

    let unsigned_id = Uuid::new_v4();
    let unsigned = LegacyV2ThoughtRecord {
        schema_version: 2,
        id: unsigned_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v2-signed-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Insight,
        role: ThoughtRole::Memory,
        content: "V2 unsigned thought".to_string(),
        confidence: Some(0.7),
        importance: 0.6,
        tags: vec!["unsigned".to_string()],
        concepts: vec!["signing".to_string()],
        refs: vec![0],
        relations: vec![],
        prev_hash: "v2-signed-hash-0".to_string(),
        hash: "v2-signed-hash-1".to_string(),
    };

    let mut bytes = Vec::new();
    for thought in [&signed, &unsigned] {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(
        chain.thoughts()[0].signing_key_id,
        Some("key-ed25519-001".to_string())
    );
    assert_eq!(chain.thoughts()[0].thought_signature, Some(vec![1, 2, 3]));
    assert!(chain.thoughts()[1].signing_key_id.is_none());
    assert!(chain.thoughts()[1].thought_signature.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_v1_chain_with_relations() {
    let dir = unique_chain_dir();
    let chain_key = "v1-with-relations";
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(chain_storage_filename(
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let first_id = Uuid::new_v4();
    let target_id = Uuid::new_v4();
    let first = LegacyThoughtV0Record {
        schema_version: 1,
        id: first_id,
        index: 0,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v1-rel-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::FactLearned,
        role: ThoughtRole::Memory,
        content: "V1 fact with relations".to_string(),
        confidence: Some(0.9),
        importance: 0.85,
        tags: vec!["v1".to_string()],
        concepts: vec!["relations".to_string()],
        refs: vec![],
        relations: vec![],
        prev_hash: String::new(),
        hash: "v1-rel-hash-0".to_string(),
    };

    let second_id = Uuid::new_v4();
    let second = LegacyThoughtV0Record {
        schema_version: 1,
        id: second_id,
        index: 1,
        timestamp: chrono::Utc::now(),
        session_id: None,
        agent_id: "v1-rel-agent".to_string(),
        signing_key_id: None,
        thought_signature: None,
        thought_type: ThoughtType::Correction,
        role: ThoughtRole::Memory,
        content: "V1 correction with relations".to_string(),
        confidence: Some(0.8),
        importance: 0.7,
        tags: vec!["v1".to_string(), "correction".to_string()],
        concepts: vec!["relations".to_string()],
        refs: vec![0],
        relations: vec![
            LegacyTestRelation {
                kind: ThoughtRelationKind::Corrects,
                target_id: first_id,
            },
            LegacyTestRelation {
                kind: ThoughtRelationKind::References,
                target_id,
            },
            LegacyTestRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: first_id,
            },
        ],
        prev_hash: "v1-rel-hash-0".to_string(),
        hash: "v1-rel-hash-1".to_string(),
    };

    let mut bytes = Vec::new();
    for thought in [&first, &second] {
        let payload = bincode::serde::encode_to_vec(thought, bincode::config::standard()).unwrap();
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    std::fs::write(&path, bytes).unwrap();

    let chain = MentisDb::open_with_key(&dir, chain_key).unwrap();
    assert_eq!(chain.thoughts().len(), 2);
    assert_eq!(chain.thoughts()[0].schema_version, MENTISDB_CURRENT_VERSION);
    assert_eq!(chain.thoughts()[1].schema_version, MENTISDB_CURRENT_VERSION);
    assert!(chain.thoughts()[0].relations.is_empty());
    assert_eq!(chain.thoughts()[1].relations.len(), 3);

    assert_eq!(
        chain.thoughts()[1].relations[0].kind,
        ThoughtRelationKind::Corrects
    );
    assert_eq!(chain.thoughts()[1].relations[0].target_id, first_id);
    assert!(chain.thoughts()[1].relations[0].chain_key.is_none());
    assert!(chain.thoughts()[1].relations[0].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[0].invalid_at.is_none());

    assert_eq!(
        chain.thoughts()[1].relations[1].kind,
        ThoughtRelationKind::References
    );
    assert_eq!(chain.thoughts()[1].relations[1].target_id, target_id);
    assert!(chain.thoughts()[1].relations[1].chain_key.is_none());
    assert!(chain.thoughts()[1].relations[1].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[1].invalid_at.is_none());

    assert_eq!(
        chain.thoughts()[1].relations[2].kind,
        ThoughtRelationKind::DerivedFrom
    );
    assert_eq!(chain.thoughts()[1].relations[2].target_id, first_id);
    assert!(chain.thoughts()[1].relations[2].chain_key.is_none());
    assert!(chain.thoughts()[1].relations[2].valid_at.is_none());
    assert!(chain.thoughts()[1].relations[2].invalid_at.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}
