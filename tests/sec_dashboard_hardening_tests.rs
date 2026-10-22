#![cfg(feature = "server")]

//! Dashboard hardening: registry storage_location trust, `.env` permissions and
//! generic error bodies.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_dash_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
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

async fn request(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// Create chain `key` in `dir` (registering it) and return its file path.
fn make_chain(dir: &Path, key: &str) -> PathBuf {
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(ThoughtType::Finding, "hello from the chain"),
        )
        .unwrap();
    drop(chain);
    dir.join(chain_storage_filename(key, StorageAdapterKind::Binary))
}

fn point_registry_at(dir: &Path, key: &str, location: &Path) {
    let path = dir.join("mentisdb-registry.json");
    let mut registry: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    registry["chains"][key]["storage_location"] = json!(location.to_string_lossy());
    std::fs::write(&path, serde_json::to_vec(&registry).unwrap()).unwrap();
}

fn thought_count(status: StatusCode, body: &str) -> u64 {
    assert_eq!(status, StatusCode::OK, "{body}");
    serde_json::from_str::<Value>(body).unwrap()["thought_count"]
        .as_u64()
        .unwrap()
}

/// A registry entry pointing outside the mentisdb directory is not followed.
#[tokio::test]
async fn registry_location_outside_dir_is_ignored() {
    let dir = fresh_dir("in");
    let outside = fresh_dir("out");
    let file = make_chain(&dir, "victim");
    let moved = outside.join("victim.tcbin");
    std::fs::rename(&file, &moved).unwrap();
    point_registry_at(&dir, "victim", &moved);

    let app = router(&dir);
    let (status, body) = request(&app, get("/dashboard/api/chains/victim")).await;
    assert_eq!(
        thought_count(status, &body),
        0,
        "outside file served: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A symlink inside the directory that escapes it is not followed either.
#[cfg(unix)]
#[tokio::test]
async fn registry_location_symlink_escape_is_ignored() {
    let dir = fresh_dir("in");
    let outside = fresh_dir("out");
    let file = make_chain(&dir, "victim");
    let moved = outside.join("victim.tcbin");
    std::fs::rename(&file, &moved).unwrap();
    let link = dir.join("link.tcbin");
    std::os::unix::fs::symlink(&moved, &link).unwrap();
    point_registry_at(&dir, "victim", &link);

    let app = router(&dir);
    let (status, body) = request(&app, get("/dashboard/api/chains/victim")).await;
    assert_eq!(
        thought_count(status, &body),
        0,
        "symlink escape followed: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&outside);
}

/// A location inside the directory is still honoured.
#[tokio::test]
async fn registry_location_inside_dir_is_honoured() {
    let dir = fresh_dir("in");
    let file = make_chain(&dir, "alpha");
    // Spell the location through a non-canonical path (`dir/sub/..`).
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let indirect = sub.join("..").join(file.file_name().unwrap());
    point_registry_at(&dir, "alpha", &indirect);

    let app = router(&dir);
    let (status, body) = request(&app, get("/dashboard/api/chains/alpha")).await;
    assert_eq!(thought_count(status, &body), 1, "{body}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Saving settings creates `.env` as 0600 and tightens an existing 0644 file.
#[cfg(unix)]
#[tokio::test]
async fn settings_env_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fresh_dir("env");
    let env_path = dir.join(".env");
    let app = router(&dir);
    let save = |value: &str| {
        Request::builder()
            .method("POST")
            .uri("/dashboard/api/settings")
            .header("content-type", "application/json")
            .header("x-mentisdb-dashboard", "1")
            .body(Body::from(
                json!({"settings": {"MENTISDB_VERBOSE": value}}).to_string(),
            ))
            .unwrap()
    };
    std::env::remove_var("MENTISDB_VERBOSE");
    let (status, body) = request(&app, save("false")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let (status, body) = request(&app, save("true")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Internal and not-found errors do not echo raw error text or host paths.
#[tokio::test]
async fn error_bodies_do_not_leak_internal_detail() {
    let dir = fresh_dir("err");
    // Corrupt registry: load fails with an error that names the file path.
    std::fs::write(dir.join("mentisdb-registry.json"), b"{ not json").unwrap();
    let app = router(&dir);
    let (status, body) = request(&app, get("/dashboard/api/chains/anything")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body.contains("internal server error"), "{body}");
    assert!(
        !body.contains(dir.to_string_lossy().as_ref()) && !body.contains("expected"),
        "leaked: {body}"
    );

    let (status, body) = request(&app, get("/dashboard/api/skills/nope")).await;
    assert!(status == StatusCode::NOT_FOUND || status == StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !body.contains(dir.to_string_lossy().as_ref()),
        "leaked: {body}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
