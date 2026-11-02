//! Harness for `compact: true` on ranked and federated search.
//!
//! A deterministic corpus across three chains (long content, typed and
//! cross-chain relations, refs, concepts, confidence, entity types, agent
//! registry fields, superseded thoughts) is searched through a matrix of
//! queries, each run in full and compact form over REST and once over MCP.
//! For every hit the harness checks that the compact form is exactly the full
//! form minus the documented dropped fields and empty values: same thoughts,
//! same order, same total scores, and graph provenance on graph-expanded hits
//! only. It also checks that the matrix exercised every kind of field it
//! claims to keep, and that compact responses are at least 15% smaller in
//! aggregate. Run with `--nocapture` to print the per-query size table.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use tower::util::ServiceExt;

/// Thought fields compact search leaves out (mirrors the server's list).
const DROPPED_THOUGHT_FIELDS: [&str; 7] = [
    "hash",
    "prev_hash",
    "thought_signature",
    "signing_key_id",
    "schema_version",
    "agent_name",
    "agent_owner",
];

const GRAPH_FIELDS: [&str; 4] = [
    "graph_distance",
    "graph_seed_paths",
    "graph_relation_kinds",
    "graph_path",
];

const CHAINS: [&str; 3] = ["harness-a", "harness-b", "harness-c"];

/// Minimum aggregate size reduction the matrix must show.
const MIN_SAVING: f64 = 0.15;

const TOPICS: [&str; 6] = [
    "quokka survey",
    "rainfall records",
    "release checklist",
    "budget decision",
    "sensor calibration",
    "supplier contract",
];

const SENTENCES: [&str; 8] = [
    "The field team recorded counts at dawn and again at dusk across both transects.",
    "Results were cross-checked against last season's baseline before sign-off.",
    "A follow-up review is due once the next quarter's figures are reconciled.",
    "Two outliers were traced to a miscalibrated sensor and excluded from the totals.",
    "The decision stands unless the variance exceeds the agreed tolerance band.",
    "Owners were notified, and the change is reflected in the shared runbook.",
    "Costs stayed within budget; the contingency line was not drawn on.",
    "The supplier confirmed delivery dates in writing for the next two orders.",
];

const TYPES: [(&str, &str); 9] = [
    ("Insight", "Memory"),
    ("Decision", "Memory"),
    ("Constraint", "Memory"),
    ("FactLearned", "Memory"),
    ("LessonLearned", "Retrospective"),
    ("Mistake", "Memory"),
    ("Finding", "Memory"),
    ("Plan", "Memory"),
    ("Summary", "Checkpoint"),
];

const AGENTS: [(&str, &str, &str); 3] = [
    ("surveyor", "Field Surveyor", "ecology-team"),
    ("planner", "Release Planner", "platform-team"),
    ("auditor", "Budget Auditor", "finance-team"),
];

/// Small deterministic generator so the corpus is identical on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_compact_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

async fn send(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Vec<u8>) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> Value {
    let (status, bytes) = send(router, uri, body.clone()).await;
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{uri} {body}: {json}");
    json
}

/// Appends the corpus and returns the REST router and its storage directory.
async fn corpus(tag: &str) -> (axum::Router, PathBuf) {
    let dir = temp_dir(tag);
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        CHAINS[0],
        StorageAdapterKind::Binary,
    ));
    let mut rng = Lcg(0x5eed);
    let mut ids: Vec<Vec<String>> = vec![Vec::new(); CHAINS.len()];
    let sizes = [60usize, 30, 20];
    for (chain_index, chain) in CHAINS.iter().enumerate() {
        for n in 0..sizes[chain_index] {
            let topic = TOPICS[rng.next(TOPICS.len())];
            let (thought_type, role) = TYPES[rng.next(TYPES.len())];
            let (agent_id, agent_name, agent_owner) = AGENTS[rng.next(AGENTS.len())];
            let mut content = format!("{topic}: note {n} on {chain}. ");
            for _ in 0..(2 + rng.next(9)) {
                content.push_str(SENTENCES[rng.next(SENTENCES.len())]);
                content.push(' ');
            }
            let first_word = topic.split(' ').next().unwrap();
            let team_tag = ["field", "ops", "finance"][rng.next(3)];
            let mut body = json!({
                "chain_key": chain,
                "agent_id": agent_id,
                "agent_name": agent_name,
                "agent_owner": agent_owner,
                "thought_type": thought_type,
                "role": role,
                "content": content.trim_end(),
                "importance": 0.3 + 0.1 * rng.next(7) as f64,
                "tags": [first_word, team_tag],
            });
            if rng.next(3) > 0 {
                body["concepts"] = json!([topic, format!("{topic} review")]);
            }
            if rng.next(2) == 0 {
                body["confidence"] = json!(0.5 + 0.05 * rng.next(10) as f64);
            }
            if rng.next(4) == 0 {
                let entity_type = ["site", "release", "contract"][rng.next(3)];
                body["entity_type"] = json!(entity_type);
            }
            let earlier = &ids[chain_index];
            if !earlier.is_empty() {
                let mut relations = Vec::new();
                let target = &earlier[rng.next(earlier.len())];
                let kind = ["References", "References", "Supersedes", "Corrects"][rng.next(4)];
                relations.push(json!({"kind": kind, "target_id": target}));
                if chain_index > 0 && rng.next(3) == 0 {
                    let cross = &ids[0][rng.next(ids[0].len())];
                    relations.push(
                        json!({"kind": "References", "target_id": cross, "chain_key": CHAINS[0]}),
                    );
                }
                body["relations"] = json!(relations);
                if rng.next(3) == 0 {
                    body["refs"] = json!([rng.next(earlier.len()) as u64]);
                }
            }
            let appended = post(&app, "/v1/thoughts", body).await;
            ids[chain_index].push(appended["thought"]["id"].as_str().unwrap().to_string());
        }
    }
    (app, dir)
}

fn is_empty_json(value: &Value) -> bool {
    matches!(value, Value::Null)
        || value == &json!("")
        || value == &json!([])
        || value == &json!({})
}

/// The compact form of a full thought: the full thought minus the dropped
/// fields and every null or empty value.
fn expected_compact_thought(full: &Value) -> Value {
    let compact: Map<String, Value> = full
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, value)| {
            !DROPPED_THOUGHT_FIELDS.contains(&key.as_str()) && !is_empty_json(value)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Value::Object(compact)
}

#[derive(Default)]
struct Coverage {
    hits: usize,
    relations: usize,
    cross_chain_relations: usize,
    superseding_relations: usize,
    concepts: usize,
    refs: usize,
    entity_types: usize,
    expanded_graph_hits: usize,
    version_annotated: usize,
}

/// Checks one full/compact response pair and adds what it covered.
fn check_pair(label: &str, full: &Value, compact: &Value, coverage: &mut Coverage) {
    assert_eq!(full["backend"], compact["backend"], "{label}");
    assert_eq!(full["total"], compact["total"], "{label}");
    let full_hits = full["results"].as_array().unwrap();
    let compact_hits = compact["results"].as_array().unwrap();
    assert_eq!(full_hits.len(), compact_hits.len(), "{label}");
    for (f, c) in full_hits.iter().zip(compact_hits) {
        coverage.hits += 1;
        assert_eq!(f["chain_key"], c["chain_key"], "{label}");
        assert_eq!(f["score"]["total"].as_f64(), c["score"].as_f64(), "{label}");
        assert_eq!(
            expected_compact_thought(&f["thought"]),
            c["thought"],
            "{label}: compact thought is not the full thought minus dropped fields"
        );
        let expanded = !f["graph_distance"].is_null();
        let mut allowed: BTreeSet<&str> = BTreeSet::from(["chain_key", "score", "thought"]);
        if expanded {
            allowed.extend(GRAPH_FIELDS);
            for key in GRAPH_FIELDS {
                assert_eq!(f[key], c[key], "{label}: {key} changed");
            }
            if f["graph_distance"].as_u64().unwrap_or(0) > 0 {
                coverage.expanded_graph_hits += 1;
            }
        }
        let keys: BTreeSet<&str> = c.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, allowed, "{label}: unexpected hit fields");

        let thought = &f["thought"];
        let relations = thought["relations"].as_array().cloned().unwrap_or_default();
        coverage.relations += relations.len();
        coverage.cross_chain_relations += relations
            .iter()
            .filter(|relation| relation.get("chain_key").is_some())
            .count();
        coverage.superseding_relations += relations
            .iter()
            .filter(|relation| relation["kind"] == "Supersedes")
            .count();
        coverage.concepts += thought["concepts"].as_array().map_or(0, Vec::len);
        coverage.refs += thought["refs"].as_array().map_or(0, Vec::len);
        coverage.entity_types += usize::from(thought["entity_type"].is_string());
        coverage.version_annotated += usize::from(
            thought.get("version_status").is_some() || thought.get("successor_of").is_some(),
        );
    }
}

fn with_extra(mut body: Value, extra: &Value) -> Value {
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn query_matrix() -> Vec<(&'static str, String, Value)> {
    let ranked_options = [
        ("plain", json!({})),
        ("graph depth 1", json!({"graph": {"max_depth": 1}})),
        ("graph depth 2", json!({"graph": {"max_depth": 2}})),
        (
            "types filter",
            json!({"thought_types": ["Decision", "Constraint", "Plan"]}),
        ),
        ("tags filter", json!({"tags_any": ["field"]})),
        ("importance floor", json!({"min_importance": 0.6})),
        ("second page", json!({"offset": 5, "limit": 5})),
        ("with superseded", json!({"include_invalidated": true})),
    ];
    let federated_options = [
        ("plain", json!({})),
        ("graph", json!({"graph": {"max_depth": 1}})),
        ("with superseded", json!({"include_invalidated": true})),
    ];
    let mut matrix = Vec::new();
    for topic in TOPICS {
        for (name, extra) in &ranked_options {
            let body = json!({"chain_key": CHAINS[0], "text": topic, "limit": 10});
            matrix.push((
                "/v1/ranked-search",
                format!("ranked {name}: {topic}"),
                with_extra(body, extra),
            ));
        }
        for (name, extra) in &federated_options {
            let body = json!({"chain_keys": CHAINS, "text": topic, "limit": 10});
            matrix.push((
                "/v1/federated-search",
                format!("federated {name}: {topic}"),
                with_extra(body, extra),
            ));
        }
    }
    matrix.push((
        "/v1/ranked-search",
        "ranked heuristic (no text)".to_string(),
        json!({"chain_key": CHAINS[0], "limit": 10}),
    ));
    matrix
}

#[tokio::test]
async fn compact_search_harness_over_generated_corpus() {
    let (app, dir) = corpus("matrix").await;
    let mut coverage = Coverage::default();
    let (mut full_bytes, mut compact_bytes) = (0usize, 0usize);
    let mut rows = Vec::new();

    for (uri, label, body) in query_matrix() {
        let (status, full_raw) = send(&app, uri, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{label}");
        let compact_body = with_extra(body, &json!({"compact": true}));
        let (status, compact_raw) = send(&app, uri, compact_body).await;
        assert_eq!(status, StatusCode::OK, "{label}");
        let full: Value = serde_json::from_slice(&full_raw).unwrap();
        let compact: Value = serde_json::from_slice(&compact_raw).unwrap();
        check_pair(&label, &full, &compact, &mut coverage);
        full_bytes += full_raw.len();
        compact_bytes += compact_raw.len();
        rows.push(format!(
            "{:>7} {:>7} {:>5.1}%  {label}",
            full_raw.len(),
            compact_raw.len(),
            100.0 * (1.0 - compact_raw.len() as f64 / full_raw.len().max(1) as f64)
        ));
    }

    let saving = 1.0 - compact_bytes as f64 / full_bytes as f64;
    eprintln!("   full compact  saved  query");
    eprintln!("{}", rows.join("\n"));
    eprintln!(
        "TOTAL {full_bytes} -> {compact_bytes} bytes, {:.1}% smaller over {} hits",
        100.0 * saving,
        coverage.hits
    );
    eprintln!(
        "covered: {} relations ({} cross-chain, {} Supersedes), {} concepts, {} refs, \
         {} entity types, {} hits reached through links, {} hits with version fields",
        coverage.relations,
        coverage.cross_chain_relations,
        coverage.superseding_relations,
        coverage.concepts,
        coverage.refs,
        coverage.entity_types,
        coverage.expanded_graph_hits,
        coverage.version_annotated
    );

    assert!(coverage.hits > 300, "too few hits: {}", coverage.hits);
    assert!(coverage.relations > 0, "no relations exercised");
    assert!(
        coverage.cross_chain_relations > 0,
        "no cross-chain relations exercised"
    );
    assert!(
        coverage.superseding_relations > 0,
        "no Supersedes relations exercised"
    );
    assert!(coverage.concepts > 0, "no concepts exercised");
    assert!(coverage.refs > 0, "no refs exercised");
    assert!(coverage.entity_types > 0, "no entity types exercised");
    assert!(
        coverage.expanded_graph_hits > 0,
        "no graph-expanded hits exercised"
    );
    assert!(
        coverage.version_annotated > 0,
        "no hits with version fields exercised"
    );
    assert!(
        saving >= MIN_SAVING,
        "compact saved {:.1}%, below the {:.0}% floor",
        100.0 * saving,
        100.0 * MIN_SAVING
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_compact_search_matches_mcp_full() {
    let (_, dir) = corpus("mcp").await;
    let mcp = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        CHAINS[0],
        StorageAdapterKind::Binary,
    ));
    // Full and compact are both read over MCP: MCP and REST format the same
    // f32 score with different precision, so each transport is compared with
    // itself.
    for (tool, parameters) in [
        (
            "mentisdb_ranked_search",
            json!({"chain_key": CHAINS[0], "text": "budget decision", "limit": 10,
                   "graph": {"max_depth": 1}}),
        ),
        (
            "mentisdb_federated_search",
            json!({"chain_keys": CHAINS, "text": "quokka survey", "limit": 10}),
        ),
    ] {
        let call = |parameters: Value| json!({"tool": tool, "parameters": parameters});
        let full = post(&mcp, "/tools/execute", call(parameters.clone())).await;
        let compact = post(
            &mcp,
            "/tools/execute",
            call(with_extra(parameters, &json!({"compact": true}))),
        )
        .await;
        let mut coverage = Coverage::default();
        check_pair(
            tool,
            &full["result"]["output"],
            &compact["result"]["output"],
            &mut coverage,
        );
        assert!(coverage.hits > 0, "{tool} returned no hits");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn default_and_compact_false_keep_the_full_form() {
    let (app, dir) = corpus("default").await;
    let query = json!({"chain_key": CHAINS[0], "text": "quokka survey", "limit": 10});
    let default = post(&app, "/v1/ranked-search", query.clone()).await;
    let explicit = post(
        &app,
        "/v1/ranked-search",
        with_extra(query, &json!({"compact": false})),
    )
    .await;
    assert_eq!(default["results"], explicit["results"]);
    let hits = default["results"].as_array().unwrap();
    assert!(!hits.is_empty());
    for hit in hits {
        assert!(hit["score"]["total"].is_number(), "{hit}");
        assert!(hit.get("matched_terms").is_some(), "{hit}");
        assert!(hit["thought"].get("hash").is_some(), "{hit}");
        assert!(hit["thought"].get("agent_name").is_some(), "{hit}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn federated_compact_with_no_chains_returns_an_empty_list() {
    let (app, dir) = corpus("empty").await;
    let json = post(
        &app,
        "/v1/federated-search",
        json!({"chain_keys": [], "text": "quokka", "compact": true}),
    )
    .await;
    assert_eq!(json["results"], json!([]));
    let _ = std::fs::remove_dir_all(dir);
}
