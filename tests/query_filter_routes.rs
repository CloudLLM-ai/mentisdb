//! `source_episode` and `entity_type` combined with an index-backed filter
//! (`tags_any`, `thought_types`, `agent_ids`) over the REST and MCP routes.
//! Each route funnels through `MentisDb::query`, which used to drop the two
//! fields once an index narrowed the candidates. In-process routers only.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

const CHAIN: &str = "query-filter-chain";

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_query_filter_routes_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> Value {
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{uri} {body}: {json}");
    json
}

fn thought(content: &str, agent: &str, episode: &str, entity: &str) -> Value {
    json!({
        "chain_key": CHAIN,
        "agent_id": agent,
        "thought_type": "Summary",
        "content": content,
        "tags": ["t"],
        "source_episode": episode,
        "entity_type": entity,
    })
}

fn contents(response: &Value) -> Vec<String> {
    let hits = response
        .get("thoughts")
        .or_else(|| response["results"].as_array().map(|_| &response["results"]))
        .and_then(Value::as_array)
        .expect("thoughts or results array");
    let mut out: Vec<String> = hits
        .iter()
        .map(|hit| {
            hit.get("thought").unwrap_or(hit)["content"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    out.sort();
    out
}

/// Search bodies that all carry an index filter next to the exact-match ones.
fn combined_filters() -> Vec<Value> {
    vec![
        json!({"chain_key": CHAIN, "tags_any": ["t"], "source_episode": "E1"}),
        json!({"chain_key": CHAIN, "thought_types": ["Summary"], "source_episode": "E1"}),
        json!({"chain_key": CHAIN, "agent_ids": ["a1", "a2"], "source_episode": "E1"}),
        json!({"chain_key": CHAIN, "tags_any": ["t"], "entity_type": "alpha"}),
        json!({"chain_key": CHAIN, "agent_ids": ["a1", "a2"], "entity_type": "alpha"}),
    ]
}

#[tokio::test]
async fn rest_search_and_ranked_apply_exact_fields_with_index_filters() {
    let dir = temp_dir("rest");
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        CHAIN,
        StorageAdapterKind::Binary,
    ));
    post(&app, "/v1/thoughts", thought("first", "a1", "E1", "alpha")).await;
    post(&app, "/v1/thoughts", thought("second", "a2", "E2", "beta")).await;

    for body in combined_filters() {
        let plain = post(&app, "/v1/search", body.clone()).await;
        assert_eq!(contents(&plain), ["first"], "search {body}");
        let ranked = post(&app, "/v1/ranked-search", body.clone()).await;
        assert_eq!(contents(&ranked), ["first"], "ranked {body}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_search_applies_exact_fields_with_index_filters() {
    let dir = temp_dir("mcp");
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        CHAIN,
        StorageAdapterKind::Binary,
    ));
    for body in [
        thought("first", "a1", "E1", "alpha"),
        thought("second", "a2", "E2", "beta"),
    ] {
        post(
            &router,
            "/tools/execute",
            json!({"tool": "mentisdb_append", "parameters": body}),
        )
        .await;
    }
    for body in combined_filters() {
        let reply = post(
            &router,
            "/tools/execute",
            json!({"tool": "mentisdb_search", "parameters": body}),
        )
        .await;
        assert_eq!(contents(&reply["result"]["output"]), ["first"], "{body}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
