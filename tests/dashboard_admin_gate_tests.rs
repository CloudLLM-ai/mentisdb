#![cfg(feature = "server")]
//! A dashboard with no PIN trusts whoever reaches its port, which is not enough
//! for a daemon that holds private chains: it neither shows them, nor deletes
//! them, nor lets the visitor change the daemon's security settings, mint
//! bearer tokens or restart it. With a PIN (the operator signed in) all of it
//! works, and a daemon without private chains keeps its old behaviour.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
pub use mentisdb::access;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
pub use mentisdb::search;
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, ManagedVectorProviderKind, MentisDb, PublicKeyAlgorithm,
    RankedSearchGraph, RankedSearchHit, RankedSearchQuery, SkillFormat, SkillRegistry, SkillUpload,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_admin_gate_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A private chain `vault`: one public thought, then the operator makes it an
/// allowlist chain.
fn private_vault(dir: &std::path::Path) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "public before")
        .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: access::AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    chain.flush().unwrap();
}

/// A public chain `notes`.
fn public_notes(dir: &std::path::Path) {
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, "notes", StorageAdapterKind::Binary).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "plain note")
        .unwrap();
    chain.flush().unwrap();
}

fn dashboard(dir: &std::path::Path, pin: Option<&str>) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.to_path_buf(),
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

async fn dash(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
    pin: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1");
    if let Some(pin) = pin {
        builder = builder.header("authorization", format!("Bearer {pin}"));
    }
    let mut request = builder.body(Body::from(body.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    // The dashboard is only ever served over TLS; the listener marks it so.
    request
        .extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Regression: `DELETE /dashboard/api/chains/{key}` never opened the chain, so
/// it skipped the private-chain PIN gate every other chain route applies.
#[tokio::test]
async fn a_no_pin_dashboard_cannot_delete_a_private_chain() {
    let dir = dir("delete");
    private_vault(&dir);
    let router = dashboard(&dir, None);
    let (status, reply) = dash(
        &router,
        "DELETE",
        "/dashboard/api/chains/vault",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{reply}");
    assert!(
        load_registered_chains(&dir)
            .unwrap()
            .chains
            .contains_key("vault"),
        "the chain is still registered"
    );
    let file = dir.join(chain_storage_filename("vault", StorageAdapterKind::Binary));
    assert!(file.exists(), "the chain file is still there");

    // The operator, with the PIN, may delete it.
    let router = dashboard(&dir, Some("2468"));
    let (status, reply) = dash(
        &router,
        "DELETE",
        "/dashboard/api/chains/vault",
        Value::Null,
        Some("2468"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(!load_registered_chains(&dir)
        .unwrap()
        .chains
        .contains_key("vault"));
}

/// A public chain is deleted without a PIN, as before, even while a private
/// chain exists: the gate is per chain.
#[tokio::test]
async fn a_no_pin_dashboard_still_deletes_a_public_chain() {
    let dir = dir("delete_public");
    public_notes(&dir);
    private_vault(&dir);
    let router = dashboard(&dir, None);
    let (status, reply) = dash(
        &router,
        "DELETE",
        "/dashboard/api/chains/notes",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    // An unknown chain is not a gate matter: it behaves as it always did.
    let (status, _) = dash(
        &router,
        "DELETE",
        "/dashboard/api/chains/never-existed",
        Value::Null,
        None,
    )
    .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}

/// Settings writes, bearer-token management and restart are admin routes: on a
/// no-PIN dashboard they are refused while the daemon holds a private chain.
#[tokio::test]
async fn admin_routes_need_the_pin_while_private_chains_exist() {
    let dir = dir("admin");
    private_vault(&dir);
    let router = dashboard(&dir, None);
    for (method, uri, body) in [
        (
            "POST",
            "/dashboard/api/settings",
            json!({"settings": {"MENTISDB_BEARER_TOKEN_ACCESS": "false"}}),
        ),
        (
            "POST",
            "/dashboard/api/bearer-tokens",
            json!({"alias": "mine", "scope": "global"}),
        ),
        (
            "POST",
            "/dashboard/api/bearer-tokens/mine/revoke",
            Value::Null,
        ),
        ("DELETE", "/dashboard/api/bearer-tokens/mine", Value::Null),
        ("POST", "/dashboard/api/restart", Value::Null),
    ] {
        let (status, reply) = dash(&router, method, uri, body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {reply}");
        assert!(
            reply["error"].as_str().unwrap_or("").contains("PIN"),
            "{method} {uri}: {reply}"
        );
    }
    // Reads are not admin changes: the token list still answers.
    let (status, _) = dash(
        &router,
        "GET",
        "/dashboard/api/bearer-tokens",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// With a PIN the same routes are not refused by the gate (they may fail for
/// their own reasons, such as an unknown alias), and a daemon with no private
/// chain keeps the old no-PIN behaviour.
#[tokio::test]
async fn admin_routes_are_open_with_a_pin_or_without_private_chains() {
    let with_pin = dir("admin_pin");
    private_vault(&with_pin);
    let router = dashboard(&with_pin, Some("2468"));
    let (status, reply) = dash(
        &router,
        "POST",
        "/dashboard/api/bearer-tokens",
        json!({"alias": "mine", "scope": "global"}),
        Some("2468"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    let public_only = dir("admin_public");
    public_notes(&public_only);
    let router = dashboard(&public_only, None);
    let (status, reply) = dash(
        &router,
        "POST",
        "/dashboard/api/bearer-tokens",
        json!({"alias": "mine", "scope": "global"}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}
