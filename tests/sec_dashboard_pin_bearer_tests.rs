#![cfg(feature = "server")]

//! The dashboard PIN must be rate limited on the `Authorization: Bearer <pin>`
//! path as well as on the login form: otherwise every dashboard route is an
//! unlimited PIN-guessing oracle.

use std::sync::atomic::AtomicBool;
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
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

const PIN: &str = "482913";

fn router() -> (axum::Router, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_pin_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let app = dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::<String, Arc<RwLock<MentisDb>>>::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: Some(PIN.to_string()),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    });
    (app, dir)
}

async fn with_bearer(app: &axum::Router, pin: &str) -> StatusCode {
    let request = Request::builder()
        .uri("/dashboard/api/chains")
        .header("authorization", format!("Bearer {pin}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

/// Five wrong PINs exhaust the budget; the correct PIN is then refused too,
/// until the window passes.
#[tokio::test]
async fn bearer_pin_guessing_is_rate_limited() {
    let (app, dir) = router();
    // A wrong PIN is bounced to the login page, not served.
    assert_ne!(with_bearer(&app, "000000").await, StatusCode::OK);
    for guess in ["000001", "000002", "000003", "000004"] {
        assert_ne!(
            with_bearer(&app, guess).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    assert_eq!(
        with_bearer(&app, PIN).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the correct PIN must not be testable after the budget is spent"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
