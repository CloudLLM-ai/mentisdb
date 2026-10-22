//! Mutating handlers run their disk I/O on the blocking pool; the async
//! runtime keeps making progress and results stay consistent.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tower::util::ServiceExt;

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// On a single-threaded runtime a ticker task keeps running while appends,
/// agent upserts and flushes are in flight, and every write lands.
#[tokio::test(flavor = "current_thread")]
async fn test_writes_do_not_starve_the_runtime() {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_blocking_io_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "main",
        StorageAdapterKind::Binary,
    ));

    let ticks = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = {
        let (ticks, stop) = (ticks.clone(), stop.clone());
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                ticks.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    };

    let mut tasks = Vec::new();
    for i in 0..24 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            post(
                app,
                "/v1/thoughts",
                json!({"chain_key":"main","agent_id":"a","thought_type":"Insight",
                       "content":format!("unique blocking io thought {i} {}", uuid::Uuid::new_v4())}),
            )
            .await
            .0
        }));
    }
    let upsert = tokio::spawn(post(
        app.clone(),
        "/v1/agents/upsert",
        json!({"chain_key":"main","agent_id":"worker","display_name":"Worker"}),
    ));
    let flush = tokio::spawn(post(app.clone(), "/v1/admin/flush", json!({})));

    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(upsert.await.unwrap().0, StatusCode::OK);
    assert_eq!(flush.await.unwrap().0, StatusCode::OK);
    stop.store(true, Ordering::Relaxed);
    ticker.await.unwrap();
    assert!(ticks.load(Ordering::Relaxed) > 0);

    let (status, head) = post(app, "/v1/head", json!({"chain_key":"main"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(head["thought_count"].as_u64().unwrap_or(0), 24, "{head}");
    let _ = std::fs::remove_dir_all(&dir);
}
