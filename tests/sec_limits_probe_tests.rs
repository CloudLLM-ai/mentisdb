//! Oversized paging and graph parameters must be clamped or refused, never
//! overflow or exhaust memory.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};
use serde_json::{json, Value};
use tower::util::ServiceExt;

async fn call(app: &axum::Router, uri: &str, body: Value) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 1))));
    app.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn huge_limits_do_not_panic_or_exhaust_memory() {
    let dir = std::env::temp_dir().join(format!("mentisdb_sec_limits_{}", uuid::Uuid::new_v4()));
    let mut chain = MentisDb::open_with_key(&dir, "main").unwrap();
    for i in 0..5 {
        chain
            .append("a", ThoughtType::Finding, &format!("thought number {i}"))
            .unwrap();
    }
    drop(chain);
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "main",
        StorageAdapterKind::Binary,
    ));
    let max = u64::MAX;
    let probes = [
        ("/v1/search", json!({"limit": max})),
        (
            "/v1/lexical-search",
            json!({"text":"thought","limit": max,"offset": max}),
        ),
        (
            "/v1/ranked-search",
            json!({"text":"thought","limit": max,"offset": max}),
        ),
        ("/v1/ranked-search", json!({"text":"thought","limit": max})),
        (
            "/v1/ranked-search",
            json!({"text":"thought","graph":{"max_depth": max,"max_visited": max}}),
        ),
        ("/v1/recent-context", json!({"last_n": max})),
        (
            "/v1/context-bundles",
            json!({"text":"thought","limit": max}),
        ),
        ("/v1/summary-candidates", json!({"limit": max})),
        (
            "/v1/thoughts/traverse",
            json!({"anchor_index":0,"max_results": max,"chunk_size": max}),
        ),
    ];
    for (route, body) in probes {
        let (app2, body2) = (app.clone(), body.clone());
        let joined = tokio::spawn(async move { call(&app2, route, body2).await }).await;
        let status = joined.unwrap_or_else(|e| panic!("{route} {body} panicked: {e}"));
        assert!(
            status != StatusCode::INTERNAL_SERVER_ERROR,
            "{route} {body} -> {status}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
