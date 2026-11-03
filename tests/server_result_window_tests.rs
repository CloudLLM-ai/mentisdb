#![cfg(feature = "server")]
//! Caller-sized numbers are bounded server-side: `offset + limit` on the ranked,
//! lexical, federated and context-bundle endpoints may not exceed
//! `MAX_RESULT_WINDOW`, and a standing-context `per_type_limit` may not exceed
//! `MAX_STANDING_PER_TYPE_CAP`. Larger values are refused with `400` and a
//! message that names the maximum, on REST and on MCP alike; values at the
//! maximum still work.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig, MAX_RESULT_WINDOW};
use mentisdb::standing::MAX_STANDING_PER_TYPE_CAP;
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn config(tag: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_window_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    MentisDbServiceConfig::new(dir, "win", StorageAdapterKind::Binary)
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn seeded() -> axum::Router {
    let router = rest_router(config("rest"));
    let (status, _) = post(
        &router,
        "/v1/thoughts",
        json!({"chain_key":"win","agent_id":"a","thought_type":"Decision","content":"Use quokka tracking."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    router
}

/// The four windowed endpoints with the body each one needs.
fn endpoints() -> [(&'static str, Value); 4] {
    let base = json!({"chain_key": "win", "text": "quokka"});
    [
        ("/v1/ranked-search", base.clone()),
        ("/v1/lexical-search", base.clone()),
        ("/v1/context-bundles", base.clone()),
        (
            "/v1/federated-search",
            json!({"chain_keys": ["win"], "text": "quokka"}),
        ),
    ]
}

fn with(mut body: Value, key: &str, value: Value) -> Value {
    body[key] = value;
    body
}

/// A window one past the maximum, an absurd limit and an absurd offset are all
/// refused with 400 naming the maximum; none of them overflows.
#[tokio::test]
async fn test_oversized_result_window_is_refused_on_every_windowed_endpoint() {
    let router = seeded().await;
    for (uri, body) in endpoints() {
        let cases = [
            with(
                with(body.clone(), "offset", json!(1)),
                "limit",
                json!(MAX_RESULT_WINDOW),
            ),
            with(body.clone(), "limit", json!(MAX_RESULT_WINDOW + 1)),
            with(body.clone(), "limit", json!(u64::MAX)),
            with(
                with(body.clone(), "offset", json!(u64::MAX)),
                "limit",
                json!(u64::MAX),
            ),
            with(body.clone(), "offset", json!(MAX_RESULT_WINDOW + 1)),
        ];
        for case in cases {
            let (status, reply) = post(&router, uri, case.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {case}: {reply}");
            let message = reply["error"].as_str().unwrap();
            assert!(
                message.contains(&MAX_RESULT_WINDOW.to_string()),
                "{uri}: {message}"
            );
        }
    }
}

/// A window exactly at the maximum still works, as does the default.
#[tokio::test]
async fn test_window_at_the_maximum_is_accepted() {
    let router = seeded().await;
    for (uri, body) in endpoints() {
        for case in [
            body.clone(),
            with(body.clone(), "limit", json!(MAX_RESULT_WINDOW)),
            with(
                with(body.clone(), "offset", json!(MAX_RESULT_WINDOW - 10)),
                "limit",
                json!(10),
            ),
        ] {
            let (status, reply) = post(&router, uri, case.clone()).await;
            assert_eq!(status, StatusCode::OK, "{uri} {case}: {reply}");
        }
    }
}

/// MCP reaches the same service methods, so it refuses the same windows.
#[tokio::test]
async fn test_mcp_refuses_an_oversized_result_window() {
    let router = mcp_router(config("mcp"));
    let (status, reply) = post(
        &router,
        "/tools/execute",
        json!({"tool": "mentisdb_ranked_search", "parameters": {"chain_key": "win", "text": "quokka", "limit": MAX_RESULT_WINDOW + 1}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(
        reply.to_string().contains(&MAX_RESULT_WINDOW.to_string()),
        "{reply}"
    );
}

/// `per_type_limit` above the standing cap is refused; the cap itself works.
#[tokio::test]
async fn test_standing_per_type_limit_is_capped() {
    let router = seeded().await;
    let (status, reply) = post(
        &router,
        "/v1/standing-context",
        json!({"chain_key": "win", "per_type_limit": MAX_STANDING_PER_TYPE_CAP + 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .unwrap()
            .contains(&MAX_STANDING_PER_TYPE_CAP.to_string()),
        "{reply}"
    );
    let (status, reply) = post(
        &router,
        "/v1/standing-context",
        json!({"chain_key": "win", "per_type_limit": MAX_STANDING_PER_TYPE_CAP}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}
