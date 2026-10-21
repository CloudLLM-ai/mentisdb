//! REST bearer middleware: oversize bodies, no per-request token-file writes.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::json;
use tower::util::ServiceExt;

fn setup() -> (std::path::PathBuf, MentisDbServiceConfig, String) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_auth_mw_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let config = MentisDbServiceConfig::new(dir.clone(), "main", StorageAdapterKind::Binary)
        .with_bearer_token_access(true);
    let token = config
        .bearer_token_store
        .create("admin", BearerTokenScope::Global)
        .unwrap()
        .token;
    (dir, config, token)
}

async fn post(app: &axum::Router, token: &str, body: Body) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/head")
                .header("content-type", "application/json")
                .header("Authorization", format!("Bearer {token}"))
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// A body over the buffering limit is rejected with 413 rather than being
/// silently treated as empty.
#[tokio::test]
async fn test_oversize_body_returns_413() {
    let (dir, config, token) = setup();
    let app = rest_router(config);
    let big = format!(
        r#"{{"chain_key":"main","pad":"{}"}}"#,
        "a".repeat(1024 * 1024 + 16)
    );
    assert_eq!(
        post(&app, &token, Body::from(big)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let ok = json!({"chain_key":"main"}).to_string();
    assert_eq!(post(&app, &token, Body::from(ok)).await, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Serving authenticated requests does not rewrite the token registry.
#[tokio::test]
async fn test_requests_do_not_rewrite_token_file() {
    let (dir, config, token) = setup();
    let path = config.bearer_token_store.path().to_path_buf();
    let before = std::fs::read(&path).unwrap();
    let app = rest_router(config);
    for _ in 0..10 {
        let ok = json!({"chain_key":"main"}).to_string();
        assert_eq!(post(&app, &token, Body::from(ok)).await, StatusCode::OK);
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}
