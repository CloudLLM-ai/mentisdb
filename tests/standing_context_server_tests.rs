#![cfg(feature = "server")]
//! `mentisdb_standing_context` over REST (`POST /v1/standing-context`) and MCP
//! tool dispatch, both on in-process routers (no sockets).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use tower::util::ServiceExt;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn config(chain: &str) -> MentisDbServiceConfig {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("mentisdb_standing_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    MentisDbServiceConfig::new(dir, chain, StorageAdapterKind::Binary)
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn append(router: &axum::Router, chain: &str, kind: &str, content: &str) {
    let (status, _) = post(
        router.clone(),
        "/v1/thoughts",
        json!({"chain_key": chain, "agent_id": "a", "thought_type": kind, "content": content}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// REST returns the prompt and typed entries; non-standing types are absent and
/// caps and type filters are honored.
#[tokio::test]
async fn test_rest_standing_context_returns_prompt_and_entries() {
    let chain = "standing-rest";
    let router = rest_router(config(chain));
    append(&router, chain, "PreferenceUpdate", "User prefers vim.").await;
    append(&router, chain, "Decision", "We use Postgres.").await;
    append(&router, chain, "Finding", "Noise finding.").await;

    let (status, body) = post(
        router.clone(),
        "/v1/standing-context",
        json!({"chain_key": chain}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let prompt = body["prompt"].as_str().unwrap();
    assert!(prompt.starts_with("=== STANDING CONTEXT ==="));
    assert!(prompt.contains("User prefers vim.") && prompt.contains("We use Postgres."));
    assert!(!prompt.contains("Noise finding."));
    assert_eq!(body["entries"].as_array().unwrap().len(), 2);
    assert_eq!(body["entries"][0]["group"], "preference");

    let (_, only) = post(
        router,
        "/v1/standing-context",
        json!({"chain_key": chain, "types": ["decision"], "per_type_limit": 1}),
    )
    .await;
    assert_eq!(only["entries"].as_array().unwrap().len(), 1);
    assert_eq!(only["entries"][0]["thought_type"], "Decision");
}

/// A bad type name is a client error, an unknown chain is 404 and creates nothing.
#[tokio::test]
async fn test_rest_standing_context_rejects_bad_type_and_unknown_chain() {
    let chain = "standing-errors";
    let router = rest_router(config(chain));
    append(&router, chain, "Goal", "Ship it.").await;
    let (status, _) = post(
        router.clone(),
        "/v1/standing-context",
        json!({"chain_key": chain, "types": ["nonsense"]}),
    )
    .await;
    assert!(status.is_client_error(), "{status}");
    let (status, _) = post(
        router,
        "/v1/standing-context",
        json!({"chain_key": "ghost-chain"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The MCP tool is listed in the catalog and dispatches to the same handler.
#[tokio::test]
async fn test_mcp_standing_context_tool_is_listed_and_executes() {
    let chain = "standing-mcp";
    let cfg = config(chain);
    let rest = rest_router(cfg.clone());
    append(&rest, chain, "Constraint", "Never push on Fridays.").await;
    let mcp = mcp_router(cfg);

    let (status, listing) = post(mcp.clone(), "/tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listing.to_string().contains("mentisdb_standing_context"),
        "tool catalog lists the standing context tool"
    );

    let (status, out) = post(
        mcp,
        "/tools/execute",
        json!({"tool": "mentisdb_standing_context", "parameters": {"chain_key": chain}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["result"]["success"], true);
    assert!(out["result"]["output"]["prompt"]
        .as_str()
        .unwrap()
        .contains("Never push on Fridays."));
}
