#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::json;
use tower::util::ServiceExt;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_search_transport_contract_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn append_thought_via_rest(
    router: axum::Router,
    chain_key: &str,
    payload: serde_json::Value,
) -> serde_json::Value {
    let mut request = json!({
        "chain_key": chain_key,
    });
    request
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().unwrap().clone());
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn phase4_rest_ranked_search_contract_exposes_graph_aware_fields() {
    let dir = unique_chain_dir();
    let chain_key = "transport-ranked";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Decision",
            "content": "Latency ranking seed for transport acceptance."
        }),
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Summary",
            "content": "Supporting context linked through explicit references.",
            "refs": [0]
        }),
    )
    .await;

    let payload = json!({
        "chain_key": chain_key,
        "text": "latency ranking",
        "limit": 10,
        "offset": 0,
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/ranked-search")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(parsed["backend"], "lexical_graph");
    let results = parsed["results"].as_array().unwrap();
    assert_eq!(parsed["total"], 2);
    assert_eq!(results.len(), 2);
    assert!(results[0]["score"].is_object());
    assert!(results[0]["score"]["lexical"].is_number());
    assert!(results[0]["score"]["graph"].is_number());
    assert!(results[0]["score"]["relation"].is_number());
    assert!(results[0]["score"]["seed_support"].is_number());
    assert!(results[0]["matched_terms"].is_array());
    assert!(results[0]["match_sources"].is_array());

    let support = results
        .iter()
        .find(|hit| {
            hit["thought"]["content"] == "Supporting context linked through explicit references."
        })
        .unwrap();
    assert_eq!(support["graph_distance"], 1);
    assert_eq!(support["graph_seed_paths"], 1);
    assert!(support["graph_relation_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|kind| kind == "references"));
    assert!(support["graph_path"].is_object());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_rest_context_bundle_contract_returns_seed_anchored_groups() {
    let dir = unique_chain_dir();
    let chain_key = "transport-bundles";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Decision",
            "content": "Alpha seed for bundle zetaanchor."
        }),
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Decision",
            "content": "Beta seed for bundle zetaanchor."
        }),
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Summary",
            "content": "Alpha-only bundle support.",
            "refs": [0]
        }),
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Summary",
            "content": "Beta-only bundle support.",
            "refs": [1]
        }),
    )
    .await;

    let payload = json!({
        "chain_key": chain_key,
        "text": "zetaanchor",
        "limit": 10,
        "offset": 0,
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/context-bundles")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(parsed["total_bundles"], 2);
    assert!(parsed["consumed_hits"].as_u64().unwrap_or(0) >= 2);
    let bundles = parsed["bundles"].as_array().unwrap();
    assert_eq!(bundles.len(), 2);

    for bundle in bundles {
        assert!(bundle["seed"]["locator"].is_object());
        assert!(bundle["seed"]["lexical_score"].is_number());
        assert!(bundle["seed"]["matched_terms"].is_array());
        assert!(bundle["seed"]["thought"].is_object());
        let support = bundle["support"].as_array().unwrap();
        assert_eq!(support.len(), 1);
        assert!(support[0]["locator"].is_object());
        assert!(support[0]["thought"].is_object());
        assert_eq!(support[0]["depth"], 1);
        assert_eq!(support[0]["seed_path_count"], 1);
        assert!(support[0]["relation_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kind| kind == "references"));
        assert!(support[0]["path"].is_object());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_rest_context_bundles_report_full_total_across_pages() {
    let dir = unique_chain_dir();
    let chain_key = "transport-bundle-pages";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    for (seed_content, support_content) in [
        ("Alpha seed for bundle paging pagerank.", "Alpha support"),
        ("Beta seed for bundle paging pagerank.", "Beta support"),
        ("Gamma seed for bundle paging pagerank.", "Gamma support"),
    ] {
        let seed = append_thought_via_rest(
            router.clone(),
            chain_key,
            json!({
                "agent_id": "planner",
                "thought_type": "Decision",
                "content": seed_content
            }),
        )
        .await;
        let seed_index = seed["thought"]["index"].as_u64().unwrap();
        append_thought_via_rest(
            router.clone(),
            chain_key,
            json!({
                "agent_id": "planner",
                "thought_type": "Summary",
                "content": support_content,
                "refs": [seed_index]
            }),
        )
        .await;
    }

    let payload = json!({
        "chain_key": chain_key,
        "text": "bundle paging pagerank",
        "limit": 1,
        "offset": 1,
        "thought_types": ["Decision"],
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/context-bundles")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(parsed["total_bundles"], 3);
    let bundles = parsed["bundles"].as_array().unwrap();
    assert_eq!(bundles.len(), 1);
    assert_eq!(
        bundles[0]["seed"]["thought"]["content"],
        "Beta seed for bundle paging pagerank."
    );
    assert!(bundles[0]["support"].as_array().unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_rest_ranked_and_bundle_contracts_honor_agent_filters() {
    let dir = unique_chain_dir();
    let chain_key = "transport-agent-filters";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    let alpha_seed = append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "alpha",
            "thought_type": "Decision",
            "content": "Shared transport search topic."
        }),
    )
    .await;
    let alpha_index = alpha_seed["thought"]["index"].as_u64().unwrap();
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "alpha",
            "thought_type": "Summary",
            "content": "Alpha-only linked support.",
            "refs": [alpha_index]
        }),
    )
    .await;

    let beta_seed = append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "beta",
            "thought_type": "Decision",
            "content": "Shared transport search topic."
        }),
    )
    .await;
    let beta_index = beta_seed["thought"]["index"].as_u64().unwrap();
    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "beta",
            "thought_type": "Summary",
            "content": "Beta-only linked support.",
            "refs": [beta_index]
        }),
    )
    .await;

    let ranked_payload = json!({
        "chain_key": chain_key,
        "text": "transport search topic",
        "agent_ids": ["beta"],
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });
    let ranked_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/ranked-search")
                .header("content-type", "application/json")
                .body(Body::from(ranked_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ranked_response.status(), StatusCode::OK);
    let ranked_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(ranked_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ranked_json["total"], 2);
    assert!(ranked_json["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|hit| hit["thought"]["agent_id"] == "beta"));

    let bundle_payload = json!({
        "chain_key": chain_key,
        "text": "transport search topic",
        "agent_ids": ["beta"],
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });
    let bundle_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/context-bundles")
                .header("content-type", "application/json")
                .body(Body::from(bundle_payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bundle_response.status(), StatusCode::OK);
    let bundle_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(bundle_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let bundles = bundle_json["bundles"].as_array().unwrap();
    assert!(!bundles.is_empty());
    assert!(bundles
        .iter()
        .all(|bundle| bundle["seed"]["thought"]["agent_id"] == "beta"));
    assert!(bundles.iter().all(|bundle| {
        bundle["support"]
            .as_array()
            .unwrap()
            .iter()
            .all(|support| support["thought"]["agent_id"] == "beta")
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_mcp_tool_catalog_exposes_ranked_and_bundle_search_tools() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "transport-mcp",
        StorageAdapterKind::Jsonl,
    ));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/list")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let tools = parsed["tools"].as_array().unwrap();

    let ranked_tool = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_ranked_search")
        .expect("phase 4 MCP contract must expose mentisdb_ranked_search");
    let ranked_parameters = ranked_tool["parameters"].as_array().unwrap();
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "text"));
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "limit"));
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "offset"));
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "graph"));
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "agent_ids"));

    let bundle_tool = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_context_bundles")
        .expect("phase 4 MCP contract must expose mentisdb_context_bundles");
    let bundle_parameters = bundle_tool["parameters"].as_array().unwrap();
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "text"));
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "limit"));
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "offset"));
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "graph"));
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "agent_ids"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_mcp_execute_honors_agent_filters_for_ranked_and_bundle_search() {
    let dir = unique_chain_dir();
    let chain_key = "transport-mcp-agent-filters";
    let rest = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));
    let mcp = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    let alpha_seed = append_thought_via_rest(
        rest.clone(),
        chain_key,
        json!({
            "agent_id": "alpha",
            "thought_type": "Decision",
            "content": "Shared MCP transport topic."
        }),
    )
    .await;
    let alpha_index = alpha_seed["thought"]["index"].as_u64().unwrap();
    append_thought_via_rest(
        rest.clone(),
        chain_key,
        json!({
            "agent_id": "alpha",
            "thought_type": "Summary",
            "content": "Alpha linked note",
            "refs": [alpha_index]
        }),
    )
    .await;

    let beta_seed = append_thought_via_rest(
        rest.clone(),
        chain_key,
        json!({
            "agent_id": "beta",
            "thought_type": "Decision",
            "content": "Shared MCP transport topic."
        }),
    )
    .await;
    let beta_index = beta_seed["thought"]["index"].as_u64().unwrap();
    append_thought_via_rest(
        rest.clone(),
        chain_key,
        json!({
            "agent_id": "beta",
            "thought_type": "Summary",
            "content": "Beta linked note",
            "refs": [beta_index]
        }),
    )
    .await;

    let ranked = mcp
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_ranked_search",
                        "parameters": {
                            "chain_key": chain_key,
                            "text": "shared MCP transport topic",
                            "agent_ids": ["beta"],
                            "graph": {
                                "mode": "incoming_only",
                                "max_depth": 1
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ranked.status(), StatusCode::OK);
    let ranked_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(ranked.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let ranked_results = ranked_json["result"]["output"]["results"]
        .as_array()
        .unwrap();
    assert_eq!(ranked_json["result"]["output"]["total"], 2);
    assert!(ranked_results
        .iter()
        .all(|hit| hit["thought"]["agent_id"] == "beta"));

    let bundles = mcp
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_context_bundles",
                        "parameters": {
                            "chain_key": chain_key,
                            "text": "shared MCP transport topic",
                            "agent_ids": ["beta"],
                            "graph": {
                                "mode": "incoming_only",
                                "max_depth": 1
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bundles.status(), StatusCode::OK);
    let bundles_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(bundles.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let bundle_items = bundles_json["result"]["output"]["bundles"]
        .as_array()
        .unwrap();
    assert!(!bundle_items.is_empty());
    assert!(bundle_items
        .iter()
        .all(|bundle| bundle["seed"]["thought"]["agent_id"] == "beta"));
    assert!(bundle_items.iter().all(|bundle| {
        bundle["support"]
            .as_array()
            .unwrap()
            .iter()
            .all(|support| support["thought"]["agent_id"] == "beta")
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn phase4_transport_keeps_plain_search_compatibility() {
    let dir = unique_chain_dir();
    let chain_key = "transport-compat";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Jsonl,
    ));

    append_thought_via_rest(
        router.clone(),
        chain_key,
        json!({
            "agent_id": "planner",
            "thought_type": "Decision",
            "content": "Keep plain search stable while ranked endpoints evolve."
        }),
    )
    .await;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": chain_key,
                        "text": "plain search stable"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(parsed["thoughts"].is_array());

    let _ = std::fs::remove_dir_all(&dir);
}
