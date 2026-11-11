//! `source_episode` provenance: REST and MCP append store it, responses and
//! reads return it, it survives a reopen, and search can filter on it.
//!
//! Before this fix the REST and MCP append requests had no `source_episode`
//! field, so serde dropped it silently (the thought came back with
//! `source_episode: null`) and the search filter was never applied.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

const CHAIN: &str = "source-episode-chain";

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_source_episode_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

fn config(dir: &PathBuf) -> MentisDbServiceConfig {
    MentisDbServiceConfig::new(dir.clone(), CHAIN, StorageAdapterKind::Binary)
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

async fn mcp(router: &axum::Router, tool: &str, parameters: Value) -> Value {
    let reply = post(
        router,
        "/tools/execute",
        json!({"tool": tool, "parameters": parameters}),
    )
    .await;
    reply["result"]["output"].clone()
}

fn append_body(content: &str, source_episode: Option<&str>) -> Value {
    let mut body = json!({
        "chain_key": CHAIN,
        "agent_id": "episode-agent",
        "thought_type": "Finding",
        "content": content,
    });
    if let Some(episode) = source_episode {
        body["source_episode"] = json!(episode);
    }
    body
}

/// Contents of the thoughts in a ranked-search style response (`results[].thought`).
fn ranked_contents(response: &Value) -> Vec<String> {
    let mut out: Vec<String> = response["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|hit| hit["thought"]["content"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

/// Contents of the thoughts in a plain search response (`thoughts[]`).
fn plain_contents(response: &Value) -> Vec<String> {
    let mut out: Vec<String> = response["thoughts"]
        .as_array()
        .expect("thoughts array")
        .iter()
        .map(|thought| thought["content"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn rest_append_stores_and_returns_source_episode() {
    let dir = temp_dir("rest");
    let app = rest_router(config(&dir));
    let appended = post(
        &app,
        "/v1/thoughts",
        append_body("quokka rest provenance", Some("ticket-42")),
    )
    .await;
    assert_eq!(appended["thought"]["source_episode"], "ticket-42");

    let id = appended["thought"]["id"].as_str().unwrap().to_string();
    let read = post(
        &app,
        "/v1/thought",
        json!({"chain_key": CHAIN, "thought_id": id}),
    )
    .await;
    assert_eq!(read["thought"]["source_episode"], "ticket-42");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn source_episode_survives_reopening_the_chain() {
    let dir = temp_dir("reopen");
    let id = {
        let app = rest_router(config(&dir));
        let appended = post(
            &app,
            "/v1/thoughts",
            append_body("quokka persisted provenance", Some("batch-7")),
        )
        .await;
        appended["thought"]["id"].as_str().unwrap().to_string()
    };
    // A fresh router on the same directory reads the chain back from disk.
    let reopened = rest_router(config(&dir));
    let read = post(
        &reopened,
        "/v1/thought",
        json!({"chain_key": CHAIN, "thought_id": id}),
    )
    .await;
    assert_eq!(read["thought"]["source_episode"], "batch-7");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_append_stores_and_returns_source_episode() {
    let dir = temp_dir("mcp");
    let router = mcp_router(config(&dir));
    let output = mcp(
        &router,
        "mentisdb_append",
        append_body("quokka mcp provenance", Some("chat-2026-11-11")),
    )
    .await;
    assert_eq!(output["thought"]["source_episode"], "chat-2026-11-11");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mcp_tool_schemas_declare_source_episode() {
    let dir = temp_dir("schema");
    let router = mcp_router(config(&dir));
    let listing = post(&router, "/tools/list", json!({})).await;
    let tools = listing["tools"].as_array().expect("tools array");
    for name in [
        "mentisdb_append",
        "mentisdb_search",
        "mentisdb_ranked_search",
        "mentisdb_federated_search",
        "mentisdb_context_bundles",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("tool {name} listed"));
        assert!(
            tool.to_string().contains("source_episode"),
            "{name} does not declare source_episode: {tool}"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Appends three thoughts sharing a keyword: two episodes and one without.
async fn seed(app: &axum::Router) {
    for (content, episode) in [
        ("zebra finding from the first episode", Some("EP-1")),
        ("zebra finding from the second episode", Some("EP-2")),
        ("zebra finding with no episode at all", None),
    ] {
        post(app, "/v1/thoughts", append_body(content, episode)).await;
    }
}

#[tokio::test]
async fn search_filters_by_source_episode_over_rest() {
    let dir = temp_dir("filter_rest");
    let app = rest_router(config(&dir));
    seed(&app).await;

    let ranked = post(
        &app,
        "/v1/ranked-search",
        json!({"chain_key": CHAIN, "text": "zebra", "source_episode": "EP-1"}),
    )
    .await;
    assert_eq!(
        ranked_contents(&ranked),
        vec!["zebra finding from the first episode"]
    );

    let plain = post(
        &app,
        "/v1/search",
        json!({"chain_key": CHAIN, "text": "zebra", "source_episode": "EP-2"}),
    )
    .await;
    assert_eq!(
        plain_contents(&plain),
        vec!["zebra finding from the second episode"]
    );

    let none = post(
        &app,
        "/v1/ranked-search",
        json!({"chain_key": CHAIN, "text": "zebra", "source_episode": "EP-404"}),
    )
    .await;
    assert!(ranked_contents(&none).is_empty());

    let federated = post(
        &app,
        "/v1/federated-search",
        json!({"chain_keys": [CHAIN], "text": "zebra", "source_episode": "EP-1"}),
    )
    .await;
    assert_eq!(
        ranked_contents(&federated),
        vec!["zebra finding from the first episode"]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn search_filters_by_source_episode_over_mcp() {
    let dir = temp_dir("filter_mcp");
    let router = mcp_router(config(&dir));
    for (content, episode) in [
        ("zebra mcp finding from the first episode", Some("EP-1")),
        ("zebra mcp finding from the second episode", Some("EP-2")),
        ("zebra mcp finding with no episode", None),
    ] {
        mcp(&router, "mentisdb_append", append_body(content, episode)).await;
    }
    let ranked = mcp(
        &router,
        "mentisdb_ranked_search",
        json!({"chain_key": CHAIN, "text": "zebra", "source_episode": "EP-2"}),
    )
    .await;
    assert_eq!(
        ranked_contents(&ranked),
        vec!["zebra mcp finding from the second episode"]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn omitting_source_episode_keeps_existing_behaviour() {
    let dir = temp_dir("compat");
    let app = rest_router(config(&dir));
    seed(&app).await;

    // A client that never sends the field still appends, and reads null.
    let appended = post(
        &app,
        "/v1/thoughts",
        append_body("quokka legacy client", None),
    )
    .await;
    assert!(appended["thought"]["source_episode"].is_null());

    // Without a filter every thought is returned, with or without an episode.
    let all = post(
        &app,
        "/v1/ranked-search",
        json!({"chain_key": CHAIN, "text": "zebra", "limit": 10}),
    )
    .await;
    assert_eq!(ranked_contents(&all).len(), 3);
    let _ = std::fs::remove_dir_all(dir);
}
