//! The 10 MiB record cap reaches REST clients as `400 Bad Request`: the append
//! fails with `InvalidInput`, which `classify_service_error` maps to 400, and
//! the chain stays usable. The body goes through `/v1/import-markdown`, a
//! large-body route (the ordinary routes stop at 2 MiB). In-process router.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn import(app: &axum::Router, line: &str) -> (StatusCode, Value) {
    let body = json!({"chain_key": "size-limit-chain", "markdown": line});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/import-markdown")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn an_oversized_thought_is_a_400_and_the_chain_still_accepts_appends() {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_size_limit_rest_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "size-limit-chain",
        StorageAdapterKind::Binary,
    ));
    let huge = format!("- [#0] Decision: {}", "x".repeat(10 * 1024 * 1024 + 1));
    let (status, reply) = import(&app, &huge).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(reply.to_string().contains("too large"), "{reply}");

    let (status, reply) = import(&app, "- [#0] Decision: a small one").await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["count"], 1);
    let _ = std::fs::remove_dir_all(dir);
}
