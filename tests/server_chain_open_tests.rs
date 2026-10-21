//! Chain opening semantics: reads never create chains, creation is explicit,
//! and concurrent first-opens are single-flight.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, validate_chain_key_shape, MentisDbServiceConfig};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_chain_open_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn router(dir: &std::path::Path) -> axum::Router {
    rest_router(MentisDbServiceConfig::new(
        dir.to_path_buf(),
        "default-chain",
        StorageAdapterKind::Binary,
    ))
}

async fn post(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Read endpoints return 404 for unknown chains and persist nothing.
#[tokio::test]
async fn test_reads_on_unknown_chain_return_404_and_create_nothing() {
    let dir = temp_dir("reads");
    let app = router(&dir);
    for uri in [
        "/v1/head",
        "/v1/search",
        "/v1/ranked-search",
        "/v1/recent-context",
        "/v1/agents",
        "/v1/thought",
    ] {
        let (status, _) = post(app.clone(), uri, json!({ "chain_key": "ghost-chain" })).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    let file = dir.join(chain_storage_filename(
        "ghost-chain",
        StorageAdapterKind::Binary,
    ));
    assert!(!file.exists(), "read must not create the chain file");
    let registry = std::fs::read_to_string(dir.join("mentisdb-registry.json")).unwrap_or_default();
    assert!(!registry.contains("ghost-chain"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Append and bootstrap still create chains; reads then succeed.
#[tokio::test]
async fn test_append_and_bootstrap_create_chains_then_reads_work() {
    let dir = temp_dir("create");
    let app = router(&dir);
    let (status, _) = post(
        app.clone(),
        "/v1/thoughts",
        json!({"chain_key":"made-by-append","agent_id":"a","thought_type":"Insight","content":"hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post(
        app.clone(),
        "/v1/bootstrap",
        json!({"chain_key":"made-by-bootstrap","content":"boot"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for key in ["made-by-append", "made-by-bootstrap"] {
        let (status, _) = post(app.clone(), "/v1/head", json!({ "chain_key": key })).await;
        assert_eq!(status, StatusCode::OK, "{key}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The operator-configured default chain is readable on a fresh daemon.
#[tokio::test]
async fn test_default_chain_is_created_on_demand_for_reads() {
    let dir = temp_dir("default");
    let (status, _) = post(router(&dir), "/v1/head", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Oversized and control-character chain keys are rejected as bad requests.
#[tokio::test]
async fn test_invalid_chain_keys_are_rejected() {
    let dir = temp_dir("invalid");
    let app = router(&dir);
    let long = "x".repeat(300);
    let (status, _) = post(app.clone(), "/v1/head", json!({ "chain_key": long })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post(app, "/v1/head", json!({ "chain_key": "bad\u{0007}key" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(validate_chain_key_shape("ok-key").is_ok());
    assert!(validate_chain_key_shape("   ").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Many concurrent first appends to one new chain each land exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_first_open_is_single_flight() {
    let dir = temp_dir("race");
    let app = router(&dir);
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let app = app.clone();
            tokio::spawn(async move {
                post(
                    app,
                    "/v1/thoughts",
                    json!({"chain_key":"raced","agent_id":"a","thought_type":"Insight","content":format!("thought number {i} unique {}", uuid::Uuid::new_v4())}),
                )
                .await
                .0
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::OK);
    }
    let (status, head) = post(app, "/v1/head", json!({ "chain_key": "raced" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head["thought_count"].as_u64().unwrap_or(0), 16, "{head}");
    let _ = std::fs::remove_dir_all(&dir);
}
