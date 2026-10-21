//! Chain-scoped bearer tokens must not reach server-wide REST/MCP operations.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{
    mcp_tool_auth, rest_route_auth, rest_router, standard_mcp_router, MentisDbServiceConfig,
    OperationAuth,
};
use mentisdb::{MentisDb, StorageAdapterKind};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

struct Fixture {
    dir: PathBuf,
    config: MentisDbServiceConfig,
    scoped: String,
    global: String,
}

fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_auth_scope_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for key in ["alpha", "beta"] {
        MentisDb::open_with_key(&dir, key).unwrap();
    }
    let config = MentisDbServiceConfig::new(dir.clone(), "alpha", StorageAdapterKind::Binary)
        .with_bearer_token_access(true);
    let store = config.bearer_token_store.clone();
    let scoped = store
        .create("scoped", BearerTokenScope::chain("alpha").unwrap())
        .unwrap()
        .token;
    let global = store
        .create("admin", BearerTokenScope::Global)
        .unwrap()
        .token;
    Fixture {
        dir,
        config,
        scoped,
        global,
    }
}

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> StatusCode {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49100,
        ))));
    router.clone().oneshot(request).await.unwrap().status()
}

/// A chain-scoped token is denied every server-wide REST operation.
#[tokio::test]
async fn test_scoped_token_denied_server_wide_rest_operations() {
    let fx = fixture();
    let app = rest_router(fx.config.clone());
    let t = fx.scoped.as_str();
    let hook = json!({"url": "https://example.com/hook"});
    let denied: Vec<(&str, &str, Option<Value>)> = vec![
        ("POST", "/v1/webhooks", Some(hook.clone())),
        (
            "POST",
            "/v1/webhooks",
            Some(json!({"url":"https://example.com/hook","chain_key_filter":"beta"})),
        ),
        ("GET", "/v1/webhooks", None),
        (
            "DELETE",
            "/v1/webhooks/00000000-0000-0000-0000-000000000000",
            None,
        ),
        ("POST", "/v1/agents", Some(json!({"chain_key":"beta"}))),
        ("POST", "/v1/skills/delete", Some(json!({"skill_id":"x"}))),
        ("POST", "/v1/skills/revoke", Some(json!({"skill_id":"x"}))),
        (
            "POST",
            "/v1/skills/deprecate",
            Some(json!({"skill_id":"x"})),
        ),
        ("POST", "/v1/skills/upload", Some(json!({"content":"x"}))),
        ("POST", "/v1/admin/flush", None),
        ("GET", "/v1/chains", None),
    ];
    for (method, uri, body) in denied {
        let status = call(&app, method, uri, t, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// In-scope calls by the same token still work.
#[tokio::test]
async fn test_scoped_token_still_allowed_in_scope() {
    let fx = fixture();
    let app = rest_router(fx.config.clone());
    let t = fx.scoped.as_str();
    let ok = call(
        &app,
        "POST",
        "/v1/webhooks",
        t,
        Some(json!({"url":"https://example.com/hook","chain_key_filter":"alpha"})),
    )
    .await;
    assert_eq!(ok, StatusCode::OK);
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/agents",
            t,
            Some(json!({"chain_key":"alpha"}))
        )
        .await,
        StatusCode::OK
    );
    // Single-chain tokens get their chain injected when none is named.
    assert_eq!(
        call(&app, "POST", "/v1/agents", t, Some(json!({}))).await,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/head",
            t,
            Some(json!({"chain_key":"alpha"}))
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(call(&app, "GET", "/health", t, None).await, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// Global tokens keep access to the server-wide operations.
#[tokio::test]
async fn test_global_token_allowed_server_wide_operations() {
    let fx = fixture();
    let app = rest_router(fx.config.clone());
    let g = fx.global.as_str();
    assert_eq!(
        call(&app, "POST", "/v1/admin/flush", g, None).await,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/v1/webhooks", g, None).await,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/v1/chains", g, None).await,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/agents",
            g,
            Some(json!({"chain_key":"beta"}))
        )
        .await,
        StatusCode::OK
    );
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// MCP applies the same classes: skill mutations and webhook listing are global-only.
#[tokio::test]
async fn test_scoped_token_denied_global_mcp_tools() {
    let fx = fixture();
    let (router, _b) = standard_mcp_router(
        fx.config.clone(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    );
    for tool in [
        "mentisdb_delete_skill",
        "mentisdb_revoke_skill",
        "mentisdb_deprecate_skill",
        "mentisdb_upload_skill",
        "mentisdb_list_webhooks",
        "mentisdb_register_webhook",
    ] {
        let status = call(
            &router,
            "POST",
            "/",
            &fx.scoped,
            Some(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":tool,"arguments":{"url":"https://example.com/x"}}})),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{tool}");
    }
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// The shared table gives REST and MCP the same class for the same operation.
#[test]
fn test_shared_table_has_rest_mcp_parity_for_global_operations() {
    for (route, method, tool) in [
        ("/v1/skills/upload", "post", "mentisdb_upload_skill"),
        ("/v1/skills/delete", "post", "mentisdb_delete_skill"),
        ("/v1/skills/revoke", "post", "mentisdb_revoke_skill"),
        ("/v1/skills/deprecate", "post", "mentisdb_deprecate_skill"),
        ("/v1/webhooks", "get", "mentisdb_list_webhooks"),
        ("/v1/chains", "get", "mentisdb_list_chains"),
    ] {
        assert_eq!(
            rest_route_auth(route, method),
            OperationAuth::GlobalOnly,
            "{route}"
        );
        assert_eq!(mcp_tool_auth(tool), OperationAuth::GlobalOnly, "{tool}");
    }
    assert_eq!(
        rest_route_auth("/v1/agents", "post"),
        OperationAuth::ChainScoped
    );
    assert_eq!(
        rest_route_auth("/v1/webhooks", "post"),
        OperationAuth::WebhookRegister
    );
    assert_eq!(
        rest_route_auth("/v1/admin/flush", "post"),
        OperationAuth::GlobalOnly
    );
}
