#![cfg(feature = "server")]

//! Dashboard hardening tests: chain-key validation, pagination clamping,
//! CSRF origin checks, and settings secret redaction.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use dashmap::DashMap;
pub use mentisdb::auth;
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

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_dashboard_sec_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn router(dir: &PathBuf) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::<String, Arc<RwLock<MentisDb>>>::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

/// A JSON POST builder carrying the CSRF opt-in header for non-browser clients.
fn post(uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1")
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

fn seed_chain(dir: &PathBuf, key: &str, count: usize) {
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, key, StorageAdapterKind::Binary).unwrap();
    for i in 0..count {
        chain
            .append_thought(
                "astro",
                ThoughtInput::new(ThoughtType::Summary, format!("thought number {i}")),
            )
            .unwrap();
    }
}

#[tokio::test]
async fn invalid_chain_keys_are_rejected_with_400() {
    let dir = unique_dir();
    seed_chain(&dir, "source", 1);
    let app = router(&dir);
    let too_long = "a".repeat(129);

    for bad in [
        "bad'key",
        "has space",
        "../escape",
        "semi;colon",
        too_long.as_str(),
    ] {
        let body = json!({"chain_key": bad, "content": "hi"});
        let (status, _) = send(
            &app,
            post("/dashboard/api/chains")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "bootstrap key {bad:?}");

        let body = json!({
            "source_chain_key": "source",
            "branch_thought_id": "00000000-0000-4000-8000-000000000001",
            "branch_chain_key": bad,
        });
        let (status, _) = send(
            &app,
            post("/dashboard/api/chains/branch")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "branch key {bad:?}");
    }

    let (status, _) = send(
        &app,
        post("/dashboard/api/agents/source/astro/copy-to/bad%27key")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = send(
        &app,
        post("/dashboard/api/chains")
            .body(Body::from(
                json!({"chain_key": "good_key-1", "content": "hello"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["bootstrapped"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn huge_per_page_is_clamped_not_panicking() {
    let dir = unique_dir();
    seed_chain(&dir, "source", 3);
    let app = router(&dir);

    for uri in [
        "/dashboard/api/chains/source/thoughts?per_page=18446744073709551615&page=18446744073709551615",
        "/dashboard/api/chains/source/search?per_page=18446744073709551615&page=18446744073709551615",
        "/dashboard/api/chains/source/search?text=thought&per_page=18446744073709551615&page=18446744073709551615",
        "/dashboard/api/chains/source/search/bundles?text=thought&per_page=18446744073709551615&page=18446744073709551615",
        "/dashboard/api/chains/source/agents/astro/thoughts?per_page=18446744073709551615",
    ] {
        let (status, body) = send(
            &app,
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        if let Some(pp) = body.get("per_page") {
            assert_eq!(pp, 500, "{uri}");
        }
    }

    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/dashboard/api/chains/source/thoughts?per_page=0")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["per_page"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn csrf_guard_blocks_cross_origin_and_allows_same_origin_or_header() {
    let dir = unique_dir();
    let app = router(&dir);
    let body = json!({"chain_key": "csrf1", "content": "x"}).to_string();

    let mk = |origin: Option<&str>, referer: Option<&str>, custom: bool| {
        let mut b = Request::builder()
            .method("POST")
            .uri("/dashboard/api/chains")
            .header("host", "localhost:19999")
            .header("content-type", "application/json");
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        if let Some(r) = referer {
            b = b.header("referer", r);
        }
        if custom {
            b = b.header("x-mentisdb-dashboard", "1");
        }
        b.body(Body::from(body.clone())).unwrap()
    };

    let (status, _) = send(&app, mk(Some("http://evil.example"), None, false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A custom header does not override a mismatching Origin.
    let (status, _) = send(&app, mk(Some("http://evil.example"), None, true)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(&app, mk(None, Some("http://evil.example/page"), false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Neither header and no opt-in header.
    let (status, _) = send(&app, mk(None, None, false)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(&app, mk(Some("http://localhost:19999"), None, false)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        &app,
        mk(None, Some("http://localhost:19999/dashboard"), false),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, mk(None, None, true)).await;
    assert_eq!(status, StatusCode::OK);

    // GETs are never blocked.
    let (status, _) = send(
        &app,
        Request::builder()
            .uri("/dashboard/api/chains")
            .header("origin", "http://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn settings_never_leak_dashboard_pin_and_blank_save_keeps_it() {
    let dir = unique_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("MENTISDB_DASHBOARD_PIN", "super-secret-pin-4242");
    let app = router(&dir);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(!text.contains("super-secret-pin-4242"), "PIN leaked");
    let parsed: Value = serde_json::from_str(&text).unwrap();
    let pin = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "MENTISDB_DASHBOARD_PIN")
        .unwrap();
    assert_eq!(pin["value"], "");
    assert_eq!(pin["set"], true);

    let (status, _) = send(
        &app,
        post("/dashboard/api/settings")
            .body(Body::from(
                json!({"settings": {"MENTISDB_DASHBOARD_PIN": ""}}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::env::var("MENTISDB_DASHBOARD_PIN").unwrap(),
        "super-secret-pin-4242"
    );

    std::env::remove_var("MENTISDB_DASHBOARD_PIN");
    let _ = std::fs::remove_dir_all(&dir);
}
