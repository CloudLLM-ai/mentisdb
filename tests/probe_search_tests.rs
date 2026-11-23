//! Multi-probe recall: [`mentisdb::search::probe`], `MentisDb::query_probes`,
//! `POST /v1/probe` and the `mentisdb_probe` MCP tool.

use mentisdb::search::probe::{
    converge_probes, probe_labels, ProbeConvergenceOptions, ProbeHitInput, ProbeLabel,
    ProbeLabelKind,
};
use mentisdb::{MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtType};
use uuid::Uuid;

const CHAIN: &str = "probe";

fn term(label: &str) -> ProbeLabel {
    ProbeLabel {
        kind: ProbeLabelKind::Term,
        label: label.to_string(),
    }
}

fn hit(id: u128, labels: &[&str]) -> ProbeHitInput {
    ProbeHitInput {
        id: Uuid::from_u128(id),
        labels: labels.iter().map(|l| term(l)).collect(),
    }
}

fn names(labels: &[ProbeLabel]) -> Vec<&str> {
    labels.iter().map(|l| l.label.as_str()).collect()
}

/// Two facts that only answer the question together, linked by "Orion", plus
/// distractors and filler that keeps query terms under the gate cutoffs.
/// Returns the directory and the ids of the two facts.
fn seed() -> (tempfile::TempDir, Uuid, Uuid) {
    // Lexical only: a background embedding-model load in a test process that
    // exits while it runs crashes inside onnxruntime.
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    for i in 0..40 {
        chain
            .append_thought(
                "filler",
                ThoughtInput::new(ThoughtType::Finding, format!("Garden note {i}: tomatoes.")),
            )
            .unwrap();
    }
    let wal = chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Finding,
                "The WAL rebase regression was fixed by Orion in sidecar.rs.",
            )
            .with_tags(["wal", "scope:user"])
            .with_concepts(["vector sidecar"]),
        )
        .unwrap()
        .id;
    let retry = chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Decision,
                "For retry backoff the recommended crate is backon, per Orion.",
            )
            .with_tags(["retries"]),
        )
        .unwrap()
        .id;
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Finding,
                "Retry storms on deploy were reported by Astro.",
            ),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Finding,
                "Tuning of ef_search was done by Astro for HNSW.",
            ),
        )
        .unwrap();
    (dir, wal, retry)
}

fn probes() -> Vec<String> {
    vec![
        "who fixed the WAL rebase regression".to_string(),
        "recommended crate for retry backoff".to_string(),
    ]
}

#[test]
fn probe_labels_skip_scope_tags_sentence_capitals_and_duplicates() {
    let labels = probe_labels(
        "Deploy went fine. Then Orion moved query_intent.rs into MentisDb and orion approved.",
        &["scope:session".into(), "Deploy".into(), "ORION".into()],
        &["release".into(), "Release".into()],
        Some("incident"),
    );
    // "Deploy" and "Then" start sentences; "orion" repeats the "ORION" tag.
    assert_eq!(
        names(&labels),
        [
            "incident",
            "release",
            "Deploy",
            "ORION",
            "query_intent.rs",
            "MentisDb"
        ]
    );
    assert_eq!(labels[0].kind, ProbeLabelKind::EntityType);
    assert_eq!(labels[1].kind, ProbeLabelKind::Concept);
    assert_eq!(labels[2].kind, ProbeLabelKind::Tag);
    assert_eq!(labels[4].kind, ProbeLabelKind::Term);
}

#[test]
fn probe_labels_find_code_identifiers_at_sentence_start() {
    let labels = probe_labels(
        "compact_to_path. src/lib.rs and a::b; Ab x9.",
        &[],
        &[],
        None,
    );
    assert_eq!(names(&labels), ["compact_to_path", "src/lib.rs", "a::b"]);
}

#[test]
fn converge_ranks_hits_reached_by_more_probes_first() {
    let found = converge_probes(
        &[
            vec![hit(1, &[]), hit(2, &[]), hit(3, &[])],
            vec![hit(4, &[]), hit(3, &[])],
            vec![hit(3, &[]), hit(4, &[])],
        ],
        ProbeConvergenceOptions::default(),
    );
    let order: Vec<u128> = found.hits.iter().map(|h| h.id.as_u128()).collect();
    assert_eq!(order, [3, 4, 1, 2]);
    assert_eq!(found.hits[0].probes, [0, 1, 2]);
    assert_eq!(found.hits[0].best_rank, 0);
    assert_eq!(found.hits[1].probes, [1, 2]);
}

#[test]
fn converge_counts_a_repeated_hit_once_per_probe() {
    let found = converge_probes(
        &[vec![hit(1, &["x"]), hit(1, &["x"])]],
        ProbeConvergenceOptions::default(),
    );
    assert_eq!(found.hits.len(), 1);
    // One thought in one probe is no convergence.
    assert!(found.entities.is_empty());
}

#[test]
fn converge_entities_need_two_probes_or_two_thoughts() {
    // Multi-probe: "Solo" spans two thoughts of one probe only, so it is out.
    let found = converge_probes(
        &[
            vec![hit(1, &["Orion", "Solo"]), hit(2, &["Solo"])],
            vec![hit(3, &["Orion"])],
        ],
        ProbeConvergenceOptions::default(),
    );
    let labels: Vec<&str> = found.entities.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["Orion"]);
    assert_eq!(found.entities[0].probes, [0, 1]);
    assert_eq!(
        found.entities[0].thought_ids,
        [Uuid::from_u128(1), Uuid::from_u128(3)]
    );

    // Single probe: two thoughts sharing a label is enough.
    let found = converge_probes(
        &[vec![
            hit(1, &["Solo"]),
            hit(2, &["Solo"]),
            hit(3, &["Once"]),
        ]],
        ProbeConvergenceOptions::default(),
    );
    let labels: Vec<&str> = found.entities.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["Solo"]);
}

#[test]
fn converge_drops_labels_every_hit_carries() {
    let probes = vec![
        vec![
            hit(1, &["project", "Orion"]),
            hit(2, &["project"]),
            hit(3, &["project"]),
        ],
        vec![hit(4, &["project", "Orion"]), hit(5, &["project"])],
    ];
    let found = converge_probes(&probes, ProbeConvergenceOptions::default());
    let labels: Vec<&str> = found.entities.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Orion"],
        "a label on all 5 hits does not discriminate"
    );

    // Below the minimum hit count the rule is off.
    let options = ProbeConvergenceOptions {
        common_label_min_hits: 6,
        ..ProbeConvergenceOptions::default()
    };
    let found = converge_probes(&probes, options);
    assert!(found.entities.iter().any(|e| e.label == "project"));
}

#[test]
fn converge_caps_entities_and_support() {
    let probes = vec![
        vec![hit(1, &["a", "b", "c"]), hit(2, &["a"]), hit(3, &["a"])],
        vec![hit(4, &["a", "b", "c"])],
    ];
    let options = ProbeConvergenceOptions {
        max_entities: 2,
        max_support: 2,
        common_label_min_hits: 100,
        ..ProbeConvergenceOptions::default()
    };
    let found = converge_probes(&probes, options);
    assert_eq!(found.entities.len(), 2);
    assert_eq!(found.entities[0].label, "a");
    assert_eq!(found.entities[0].thought_ids.len(), 2);
}

#[test]
fn converge_is_deterministic_and_handles_no_probes() {
    let probes = vec![
        vec![hit(9, &["x", "y"]), hit(8, &["y", "x"])],
        vec![hit(7, &["y", "x"]), hit(6, &["x", "y"])],
    ];
    let first = converge_probes(&probes, ProbeConvergenceOptions::default());
    for _ in 0..10 {
        assert_eq!(
            converge_probes(&probes, ProbeConvergenceOptions::default()),
            first
        );
    }
    let empty = converge_probes(&[], ProbeConvergenceOptions::default());
    assert!(empty.hits.is_empty() && empty.entities.is_empty());
}

#[test]
fn query_probes_bridges_two_facts_through_their_shared_entity() {
    let (dir, wal, retry) = seed();
    let chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    let mut with_blank = probes();
    with_blank.push("   ".to_string());
    let found = chain.query_probes(
        &RankedSearchQuery::new().with_limit(5),
        &with_blank,
        ProbeConvergenceOptions::default(),
    );

    assert_eq!(found.probes.len(), 2, "blank probes are skipped");
    assert_eq!(found.probes[0].result.hits[0].thought.id, wal);
    assert_eq!(found.probes[1].result.hits[0].thought.id, retry);
    let orion = &found.convergence.entities[0];
    assert_eq!(orion.label, "Orion");
    assert_eq!(orion.probes, [0, 1]);
    assert!(orion.thought_ids.contains(&wal) && orion.thought_ids.contains(&retry));
    // Astro only shows up under the retry probe.
    assert!(!found
        .convergence
        .entities
        .iter()
        .any(|e| e.label == "Astro"));
}

#[cfg(feature = "server")]
mod server {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::{Request, StatusCode};
    use mentisdb::server::{rest_router, standard_mcp_router, MentisDbServiceConfig};
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    fn config(dir: &tempfile::TempDir) -> MentisDbServiceConfig {
        MentisDbServiceConfig::new(dir.path().to_path_buf(), CHAIN, StorageAdapterKind::Binary)
            .with_bearer_token_access(false)
    }

    async fn rest(dir: &tempfile::TempDir, body: Value) -> (StatusCode, Value) {
        let response = rest_router(config(dir))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/probe")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn mcp(dir: &tempfile::TempDir, method: &str, params: Value) -> Value {
        let (router, _events) = standard_mcp_router(
            config(dir),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        );
        let mut request = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                49004,
            ))));
        let response = router.oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()["result"].clone()
    }

    #[tokio::test]
    async fn rest_probe_returns_probes_results_and_entities() {
        let (dir, wal, retry) = seed();
        let (status, body) = rest(&dir, json!({"probes": probes(), "limit": 3})).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let reported = body["probes"].as_array().unwrap();
        assert_eq!(reported.len(), 2);
        assert_eq!(reported[0]["text"], probes()[0]);
        assert!(reported[0]["intent"]
            .as_array()
            .unwrap()
            .contains(&json!("agent")));
        assert_eq!(reported[0]["hits"][0], wal.to_string());
        assert_eq!(reported[1]["hits"][0], retry.to_string());
        assert!(reported
            .iter()
            .all(|p| p["hits"].as_array().unwrap().len() <= 3));

        let results = body["results"].as_array().unwrap();
        let ids: Vec<&str> = results
            .iter()
            .map(|r| r["thought"]["id"].as_str().unwrap())
            .collect();
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(ids.len(), distinct.len(), "each thought appears once");
        assert!(
            results[0]["thought"]["hash"].is_string(),
            "REST is full by default"
        );
        assert_eq!(results[0]["chain_key"], CHAIN);

        assert_eq!(body["entities"][0]["label"], "Orion");
        assert_eq!(body["entities"][0]["kind"], "term");
        assert_eq!(body["entities"][0]["probes"], json!([0, 1]));
    }

    #[tokio::test]
    async fn rest_probe_compact_drops_integrity_fields() {
        let (dir, _, _) = seed();
        let (status, body) = rest(&dir, json!({"probes": probes(), "compact": true})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["results"][0]["thought"].get("hash").is_none());
        assert!(body["results"][0]["thought"]["content"].is_string());
    }

    #[tokio::test]
    async fn rest_probe_rejects_bad_requests() {
        let (dir, _, _) = seed();
        let nine: Vec<String> = (0..9).map(|i| format!("clue {i}")).collect();
        for body in [
            json!({}),
            json!({"probes": []}),
            json!({"probes": ["  "]}),
            json!({"probes": nine}),
            json!({"probes": ["x".repeat(1001)]}),
            json!({"probes": ["wal"], "text": "wal"}),
            json!({"probes": ["wal"], "offset": 2}),
            json!({"probes": ["wal"], "limit": 0}),
            json!({"probes": ["wal"], "limit": 26}),
        ] {
            let (status, response) = rest(&dir, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response}");
        }
        let many_tags: Vec<String> = (0..101).map(|i| format!("t{i}")).collect();
        let (status, _) = rest(&dir, json!({"probes": ["wal"], "tags_any": many_tags})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn mcp_probe_is_listed_and_lean_by_default() {
        let (dir, _, _) = seed();
        let tools = mcp(&dir, "tools/list", json!({})).await;
        let tool = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "mentisdb_probe")
            .expect("mentisdb_probe is advertised");
        assert_eq!(tool["inputSchema"]["required"], json!(["probes"]));

        let result = mcp(
            &dir,
            "tools/call",
            json!({"name": "mentisdb_probe", "arguments": {"probes": probes()}}),
        )
        .await;
        assert_ne!(result["isError"], json!(true), "{result}");
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(body["default_provenance"].is_string());
        let first = &body["results"][0];
        assert!(first.get("chain_key").is_none(), "home chain is implied");
        assert!(first["thought"].get("hash").is_none());
        assert!(first["thought"].get("provenance").is_none());
        assert_eq!(body["entities"][0]["label"], "Orion");

        let full = mcp(
            &dir,
            "tools/call",
            json!({"name": "mentisdb_probe", "arguments": {"probes": probes(), "compact": false}}),
        )
        .await;
        let body: Value =
            serde_json::from_str(full["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(body["results"][0]["thought"]["hash"].is_string());
    }
}
