#![cfg(feature = "server")]
//! Who may listen to the MCP event stream, and what the stream carries.
//!
//! `GET /` streams one `tool_called` event per tool call on every chain (tool
//! name, chain key, outcome). A chain-scoped token must not see that: it would
//! learn which other chains exist and when they are used. Authorization of MCP
//! methods is an explicit allow-list that fails closed, and the event reports
//! only a validated chain key string and a tool-shaped name.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{standard_mcp_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower::util::ServiceExt;

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sse_scope_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary)
        .with_bearer_token_access(true)
}

async fn send(router: &Router, mut request: Request<Body>) -> (StatusCode, String) {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            44100,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn rpc(token: &str, message: Value) -> Request<Body> {
    Request::post("/")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(message.to_string()))
        .unwrap()
}

fn request(token: &str, method: &str, id: Option<u64>) -> Request<Body> {
    let mut message = json!({"jsonrpc": "2.0", "method": method, "params": {}});
    if let Some(id) = id {
        message["id"] = json!(id);
    }
    rpc(token, message)
}

async fn subscribe(router: &Router, token: &str) -> StatusCode {
    let mut request = Request::get("/")
        .header("accept", "text/event-stream")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            44101,
        ))));
    router.clone().oneshot(request).await.unwrap().status()
}

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Regression: `events/subscribe` was authorized for any active token, so a
/// token scoped to one chain could watch tool calls on every chain.
#[tokio::test]
async fn only_a_global_token_may_subscribe_to_the_event_stream() {
    let config = config("subscribe");
    let global = config
        .bearer_token_store
        .create("global", BearerTokenScope::Global)
        .unwrap()
        .token;
    let scoped = config
        .bearer_token_store
        .create("scoped", BearerTokenScope::chain("mine").unwrap())
        .unwrap()
        .token;
    let (router, _broadcaster) = standard_mcp_router(config, LOOPBACK);
    assert_eq!(subscribe(&router, &global).await, StatusCode::OK);
    assert_eq!(
        subscribe(&router, &scoped).await,
        StatusCode::UNAUTHORIZED,
        "a chain-scoped token must not see other chains' activity"
    );
    assert_eq!(
        subscribe(&router, "not-a-token").await,
        StatusCode::UNAUTHORIZED
    );
}

/// The default arm fails closed: a method the allow-list does not name is for
/// global tokens only, while the methods every MCP client sends (`initialize`,
/// `ping`, `tools/list`, `resources/list`, notifications) keep working for a
/// chain-scoped token.
#[tokio::test]
async fn unknown_methods_need_a_global_token_and_known_ones_do_not() {
    let config = config("methods");
    let global = config
        .bearer_token_store
        .create("global", BearerTokenScope::Global)
        .unwrap()
        .token;
    let scoped = config
        .bearer_token_store
        .create("scoped", BearerTokenScope::chain("mine").unwrap())
        .unwrap()
        .token;
    let (router, _broadcaster) = standard_mcp_router(config, LOOPBACK);

    for method in ["initialize", "ping", "tools/list", "resources/list"] {
        let (status, body) = send(&router, request(&scoped, method, Some(1))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{method} for a scoped token: {body}"
        );
    }
    for method in ["notifications/initialized", "notifications/cancelled"] {
        let (status, body) = send(&router, request(&scoped, method, None)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{method}: {body}");
    }
    for method in [
        "admin/everything",
        "logging/setLevel",
        "prompts/list",
        "events/subscribe",
    ] {
        let (status, _) = send(&router, request(&scoped, method, Some(2))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} must fail closed for a scoped token"
        );
        let (status, body) = send(&router, request(&global, method, Some(2))).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} is authorized for a global token: {body}"
        );
    }
}

/// Collect the `tool_called` event data produced by calling `tool` with
/// `arguments`.
async fn tool_called_event(tool: &str, arguments: Value) -> Value {
    let config = config("event");
    let global = config
        .bearer_token_store
        .create("global", BearerTokenScope::Global)
        .unwrap()
        .token;
    let (router, broadcaster) = standard_mcp_router(config, LOOPBACK);
    let mut events = broadcaster.subscribe();
    let message = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    });
    let _ = send(&router, rpc(&global, message)).await;
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("a tool_called event arrives")
            .unwrap();
        if event.event.as_deref() == Some("tool_called") {
            return event.data;
        }
    }
}

/// Regression: the event carried `chain_key` exactly as the client sent it, of
/// any JSON type and size.
#[tokio::test]
async fn the_event_reports_only_a_valid_chain_key_string() {
    let ok = tool_called_event("mentisdb_head", json!({"chain_key": "  project-x "})).await;
    assert_eq!(ok["chain_key"], "project-x", "{ok}");

    for hostile in [
        json!({"chain_key": {"nested": vec!["x"; 50]}}),
        json!({"chain_key": 42}),
        json!({"chain_key": "x".repeat(10_000)}),
        json!({"chain_key": "bad\u{7}key"}),
        json!({"chain_key": "   "}),
        json!({}),
    ] {
        let event = tool_called_event("mentisdb_head", hostile.clone()).await;
        assert_eq!(event["chain_key"], Value::Null, "{hostile} -> {event}");
    }
}

/// The tool name in the event has the shape of a tool name, whatever the
/// client called.
#[tokio::test]
async fn the_event_reports_only_a_tool_shaped_name() {
    let event = tool_called_event("mentisdb_head", json!({})).await;
    assert_eq!(event["tool"], "mentisdb_head");
    let event = tool_called_event("evil tool\nname <script>", json!({})).await;
    assert_eq!(event["tool"], "unknown", "{event}");
    let event = tool_called_event(&"a".repeat(5000), json!({})).await;
    assert_eq!(event["tool"], "unknown", "{event}");
}
