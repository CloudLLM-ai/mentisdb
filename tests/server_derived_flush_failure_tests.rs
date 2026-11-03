#![cfg(feature = "server")]
//! A failed derived-state flush after a durable append must not fail the
//! request: the thought is already in the chain, so an error would make the
//! client retry and write it twice.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::util::ServiceExt;

async fn append(app: &axum::Router, content: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"chain_key":"flush","agent_id":"t","thought_type":"Insight","content":content})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn sidecar_wal(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".json.wal"))
        .expect("the first append created the vector sidecar WAL")
}

/// Sabotaging the sidecar WAL (a directory cannot be appended to) makes the
/// post-append flush fail. The append still answers 200, the thought is stored
/// exactly once, and once the WAL is usable again the next append flushes the
/// queued record instead of losing it.
#[tokio::test]
async fn test_append_succeeds_when_the_derived_flush_fails() {
    let dir = tempfile::tempdir().unwrap();
    let app = rest_router(MentisDbServiceConfig::new(
        dir.path().to_path_buf(),
        "flush",
        StorageAdapterKind::Binary,
    ));
    assert_eq!(append(&app, "first thought").await.0, StatusCode::OK);

    let wal = sidecar_wal(dir.path());
    std::fs::remove_file(&wal).unwrap();
    std::fs::create_dir(&wal).unwrap();

    let (status, body) = append(&app, "second thought").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a durable thought must not be reported as an error: {body}"
    );
    assert_eq!(body["thought"]["content"], "second thought", "{body}");

    std::fs::remove_dir(&wal).unwrap();
    assert_eq!(append(&app, "third thought").await.0, StatusCode::OK);
    drop(app);

    let reopened = MentisDb::open_with_key(dir.path(), "flush").unwrap();
    let contents: Vec<&str> = reopened
        .thoughts()
        .iter()
        .map(|t| t.content.as_str())
        .collect();
    assert_eq!(
        contents,
        ["first thought", "second thought", "third thought"]
    );
}
