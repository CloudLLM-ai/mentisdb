//! Regression tests for the legacy `/tools/execute` authorization bypass:
//! authorization must read exactly the fields the executing handler uses.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{combined_mcp_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use tower::util::ServiceExt;

struct Fixture {
    dir: PathBuf,
    app: axum::Router,
    scoped: String,
}

fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_shape_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for key in ["alpha", "beta"] {
        MentisDb::open_with_key(&dir, key).unwrap();
    }
    let config = MentisDbServiceConfig::new(dir.clone(), "alpha", StorageAdapterKind::Binary)
        .with_bearer_token_access(true);
    let scoped = config
        .bearer_token_store
        .create("scoped", BearerTokenScope::chain("alpha").unwrap())
        .unwrap()
        .token;
    let app = combined_mcp_router(config, IpAddr::V4(Ipv4Addr::LOCALHOST));
    Fixture { dir, app, scoped }
}

async fn post(app: &axum::Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49100,
        ))));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn rpc_call(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
           "params":{"name":name,"arguments":arguments}})
}

/// Exploit: authorize `name` (in scope) while the legacy handler executes `tool`.
#[tokio::test]
async fn mismatched_name_and_tool_is_rejected() {
    let fx = fixture();
    let (status, body) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({
            "name": "mentisdb_head", "arguments": {"chain_key": "alpha"},
            "tool": "mentisdb_head", "parameters": {"chain_key": "beta"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// Exploit: global-only tool smuggled behind an in-scope `name`.
#[tokio::test]
async fn smuggled_list_chains_is_rejected() {
    let fx = fixture();
    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"name":"mentisdb_head","arguments":{"chain_key":"alpha"},
               "tool":"mentisdb_list_chains","parameters":{}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Honest direct call is denied for a scoped token (global-only).
    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_list_chains","parameters":{}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// Exploit: merge another chain into an in-scope one via the smuggled fields.
#[tokio::test]
async fn smuggled_merge_chains_is_rejected() {
    let fx = fixture();
    let merge = json!({"source_chain_key":"beta","target_chain_key":"alpha"});
    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"name":"mentisdb_head","arguments":{"chain_key":"alpha"},
               "tool":"mentisdb_merge_chains","parameters":merge}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Honest legacy call naming an out-of-scope chain is denied.
    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_merge_chains","parameters":merge}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// Legacy payload with only `name`/`arguments` has no `tool`, so it must not
/// be authorized by those fields (and carries the ambiguous shape).
#[tokio::test]
async fn legacy_route_ignores_jsonrpc_fields() {
    let fx = fixture();
    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"name":"mentisdb_head","arguments":{"chain_key":"alpha"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// JSON-RPC tools/call carrying legacy fields is rejected too.
#[tokio::test]
async fn jsonrpc_call_with_legacy_fields_is_rejected() {
    let fx = fixture();
    let (status, _) = post(
        &fx.app,
        "/",
        &fx.scoped,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"name":"mentisdb_head","arguments":{"chain_key":"alpha"},
                         "tool":"mentisdb_list_chains","parameters":{}}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// Honest in-scope calls on both transports work; out-of-scope ones do not.
#[tokio::test]
async fn honest_calls_still_work() {
    let fx = fixture();
    let (status, body) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_head","parameters":{"chain_key":"alpha"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_head","parameters":{"chain_key":"beta"}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = post(
        &fx.app,
        "/",
        &fx.scoped,
        rpc_call("mentisdb_head", json!({"chain_key":"alpha"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = post(
        &fx.app,
        "/",
        &fx.scoped,
        rpc_call("mentisdb_head", json!({"chain_key":"beta"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = post(
        &fx.app,
        "/",
        &fx.scoped,
        rpc_call("mentisdb_list_chains", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// A single-chain token omitting chain_key is still bound to its chain on both
/// transports (injection targets the executed field only).
#[tokio::test]
async fn single_chain_injection_still_applies() {
    let fx = fixture();
    let (status, body) = post(
        &fx.app,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_head","parameters":{}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = post(
        &fx.app,
        "/",
        &fx.scoped,
        rpc_call("mentisdb_head", json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = std::fs::remove_dir_all(&fx.dir);
}
