//! Regression tests for the single-chain-token scope bypass: a decoy chain
//! field (`target_chain_key`, `source_chain_key`, ...) satisfied the
//! authorizer while the handler, which only reads `chain_key`, fell back to the
//! server's default chain.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{combined_mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use tower::util::ServiceExt;

struct Fixture {
    dir: PathBuf,
    mcp: axum::Router,
    rest: axum::Router,
    scoped: String,
    /// Covers `alpha` and `beta`, not the default `secret`.
    multi: String,
}

/// Default chain is `secret`; the token only covers `alpha`.
fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_scope_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    for key in ["alpha", "secret"] {
        let mut chain = MentisDb::open_with_key(&dir, key).unwrap();
        chain
            .append(
                "seed",
                mentisdb::ThoughtType::Finding,
                &format!("marker-{key}"),
            )
            .unwrap();
    }
    let config = MentisDbServiceConfig::new(dir.clone(), "secret", StorageAdapterKind::Binary)
        .with_bearer_token_access(true);
    let scoped = config
        .bearer_token_store
        .create("scoped", BearerTokenScope::chain("alpha").unwrap())
        .unwrap()
        .token;
    let multi = config
        .bearer_token_store
        .create(
            "multi",
            BearerTokenScope::chains(["alpha", "beta"]).unwrap(),
        )
        .unwrap()
        .token;
    let mcp = combined_mcp_router(config.clone(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    let rest = rest_router(config);
    Fixture {
        dir,
        mcp,
        rest,
        scoped,
        multi,
    }
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `chain_key` of the chain a `head` call actually read.
fn head_chain(body: &Value) -> Option<String> {
    body.get("chain_key")
        .or_else(|| body.pointer("/result/output/chain_key"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

const DECOYS: [&str; 5] = [
    "target_chain_key",
    "source_chain_key",
    "branch_chain_key",
    "chain_key_filter",
    "chain_keys",
];

#[tokio::test]
async fn rest_decoy_chain_field_cannot_redirect_to_default_chain() {
    let fx = fixture();
    for decoy in DECOYS {
        let decoy_value = if decoy == "chain_keys" {
            json!(["alpha"])
        } else {
            json!("alpha")
        };
        let (status, body) = post(
            &fx.rest,
            "/v1/head",
            &fx.scoped,
            json!({ decoy: decoy_value }),
        )
        .await;
        if status == StatusCode::OK {
            assert_eq!(
                head_chain(&body).as_deref(),
                Some("alpha"),
                "decoy `{decoy}` redirected the read: {body}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&fx.dir);
}

#[tokio::test]
async fn mcp_decoy_chain_field_cannot_redirect_to_default_chain() {
    let fx = fixture();
    for decoy in DECOYS {
        let decoy_value = if decoy == "chain_keys" {
            json!(["alpha"])
        } else {
            json!("alpha")
        };
        let (status, body) = post(
            &fx.mcp,
            "/tools/execute",
            &fx.scoped,
            json!({"tool":"mentisdb_head","parameters":{ decoy: decoy_value }}),
        )
        .await;
        if status == StatusCode::OK {
            assert_eq!(
                head_chain(&body).as_deref(),
                Some("alpha"),
                "decoy `{decoy}` redirected the read: {body}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// A multi-chain token is not rewritten by the injection, so the authorizer
/// itself must require the default chain when `chain_key` is blank.
#[tokio::test]
async fn multi_chain_token_decoy_is_denied_for_default_chain_fallback() {
    let fx = fixture();
    let (status, _) = post(
        &fx.rest,
        "/v1/head",
        &fx.multi,
        json!({"target_chain_key": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = post(
        &fx.mcp,
        "/tools/execute",
        &fx.multi,
        json!({"tool":"mentisdb_head","parameters":{"chain_keys":["alpha"]}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Naming the chain properly still works.
    let (status, body) = post(
        &fx.rest,
        "/v1/head",
        &fx.multi,
        json!({"chain_key": "beta"}),
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{body}");
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// A blank `chain_key` next to a decoy is the same as an absent one.
#[tokio::test]
async fn blank_chain_key_with_decoy_cannot_reach_default_chain() {
    let fx = fixture();
    let (status, _) = post(
        &fx.rest,
        "/v1/head",
        &fx.multi,
        json!({"chain_key": "  ", "source_chain_key": "alpha"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&fx.dir);
}

/// A blank entry in `chain_keys` resolved to the default chain at execution
/// time while the authorizer skipped it, leaking the default chain.
#[tokio::test]
async fn federated_blank_chain_key_cannot_read_default_chain() {
    let fx = fixture();
    for token in [&fx.scoped, &fx.multi] {
        let (status, body) = post(
            &fx.rest,
            "/v1/federated-search",
            token,
            json!({"chain_keys": ["", "alpha"], "text": "marker"}),
        )
        .await;
        assert!(
            !body.to_string().contains("marker-secret"),
            "default chain leaked ({status}): {body}"
        );
    }
    let (status, body) = post(
        &fx.mcp,
        "/tools/execute",
        &fx.scoped,
        json!({"tool":"mentisdb_federated_search",
               "parameters":{"chain_keys":["   ","alpha"],"text":"marker"}}),
    )
    .await;
    assert!(
        !body.to_string().contains("marker-secret"),
        "default chain leaked ({status}): {body}"
    );
    let _ = std::fs::remove_dir_all(&fx.dir);
}
