#![cfg(feature = "server")]

//! Regression tests for the dashboard Settings write path
//! (`POST /dashboard/api/settings`): what it accepts, what it writes to
//! `<MENTISDB_DIR>/.env`, and that the file it writes always loads back.
//!
//! The handler mutates the process environment, so every test takes
//! [`ENV_LOCK`] and restores the variables it touched.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use dashmap::DashMap;
pub use mentisdb::access;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
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
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serialises tests that read or write process environment variables.
async fn env_guard() -> tokio::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().await
}

/// A fresh, empty MENTISDB_DIR for one test.
fn unique_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_settings_env_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn router(dir: &Path) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::<String, Arc<RwLock<MentisDb>>>::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.to_path_buf(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

/// POST `settings` and return the status and JSON body.
async fn save(dir: &Path, settings: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/dashboard/api/settings")
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1")
        .body(Body::from(json!({ "settings": settings }).to_string()))
        .unwrap();
    let resp = router(dir).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A NUL byte in a value makes `std::env::set_var` panic. The request must
/// be refused with 400 instead of unwinding inside the handler.
#[tokio::test]
async fn test_nul_in_value_is_rejected_not_a_panic() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::env::remove_var("MENTISDB_UPDATE_REPO");

    let (status, body) = save(&dir, json!({ "MENTISDB_UPDATE_REPO": "a\u{0}b" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(std::env::var("MENTISDB_UPDATE_REPO").is_err());
    assert!(!dir.join(".env").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
