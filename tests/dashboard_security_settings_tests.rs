#![cfg(feature = "server")]
//! Security-sensitive daemon settings (the webhook SSRF allow-list, the Host
//! allow-list that guards against DNS rebinding, bearer-token access, TLS
//! paths, the bind host, ...) may only be changed from a dashboard protected
//! by a PIN, with the operator signed in. A PIN-less dashboard refuses them
//! with 403 (the whole request, nothing saved) but still saves ordinary
//! settings and can bootstrap a PIN.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
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

/// Every security-sensitive setting, as the catalog must flag them. Changing
/// this list is a security decision: keep it in step with the catalog.
const SECURITY_KEYS: [(&str, &str); 9] = [
    ("MENTISDB_DIR", "/tmp/elsewhere"),
    ("MENTISDB_LOG_FILE", "/tmp/mentisdb-test.log"),
    ("MENTISDB_BIND_HOST", "127.0.0.1"),
    ("MENTISDB_TLS_CERT", "/tmp/cert.pem"),
    ("MENTISDB_TLS_KEY", "/tmp/key.pem"),
    ("MENTISDB_DASHBOARD_ALLOWED_HOSTS", "example.test"),
    ("MENTISDB_BEARER_TOKEN_ACCESS", "false"),
    ("MENTISDB_WEBHOOK_ALLOWED_CIDRS", "10.1.0.0/16"),
    ("MENTISDB_UPDATE_REPO", "someone/else"),
];

/// A fresh test directory.
fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_settings_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A dashboard over `dir` with `pin`, plus its session map so a test can sign in.
fn dashboard(
    dir: &std::path::Path,
    pin: Option<&str>,
) -> (
    axum::Router,
    Arc<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
) {
    let sessions = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let router = dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.to_path_buf(),
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: sessions.clone(),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    });
    (router, sessions)
}

/// How a request proves who it is.
enum Auth<'a> {
    Anonymous,
    Session(&'a str),
}

/// POST `settings` to the settings route and return status and JSON body.
async fn post_settings(
    router: &axum::Router,
    settings: Value,
    auth: Auth<'_>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/dashboard/api/settings")
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1");
    if let Auth::Session(token) = auth {
        builder = builder.header("cookie", format!("mentisdb_session={token}"));
    }
    let mut request = builder
        .body(Body::from(json!({ "settings": settings }).to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    request
        .extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// GET the settings list.
async fn get_settings(router: &axum::Router, auth: Auth<'_>) -> Value {
    let mut builder = Request::builder().uri("/dashboard/api/settings");
    if let Auth::Session(token) = auth {
        builder = builder.header("cookie", format!("mentisdb_session={token}"));
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    request
        .extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
    let response = router.clone().oneshot(request).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Whether `<dir>/.env` exists and mentions `name`.
fn env_file_mentions(dir: &std::path::Path, name: &str) -> bool {
    std::fs::read_to_string(dir.join(".env")).is_ok_and(|text| text.contains(name))
}

/// The catalog flags exactly the expected security-sensitive settings, and
/// leaves the PIN (the bootstrap) unflagged.
#[test]
fn test_catalog_flags_exactly_the_security_settings() {
    let mut flagged: Vec<&str> = dashboard_impl::DAEMON_SETTINGS
        .iter()
        .filter(|spec| spec.security_sensitive)
        .map(|spec| spec.name)
        .collect();
    flagged.sort_unstable();
    let mut expected: Vec<&str> = SECURITY_KEYS.iter().map(|(name, _)| *name).collect();
    expected.sort_unstable();
    assert_eq!(flagged, expected);
    assert!(
        !dashboard_impl::setting_spec("MENTISDB_DASHBOARD_PIN")
            .unwrap()
            .security_sensitive
    );
}

/// A PIN-less dashboard refuses each security-sensitive key on its own with
/// 403, names it, tells the operator what to do, and saves nothing.
#[tokio::test]
async fn test_pinless_dashboard_refuses_each_security_key() {
    let dir = dir("refuse");
    let (router, _) = dashboard(&dir, None);
    for (name, value) in SECURITY_KEYS {
        let (status, reply) = post_settings(&router, json!({ name: value }), Auth::Anonymous).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {reply}");
        let message = reply["message"].as_str().unwrap_or_default();
        assert!(message.contains(name), "{name}: {reply}");
        assert!(message.contains("MENTISDB_DASHBOARD_PIN"), "{reply}");
        assert!(message.contains(".env"), "{reply}");
        assert!(!env_file_mentions(&dir, name), "{name} was saved");
    }
}

/// Blanking a security key (unsetting it) is a change too and is refused.
#[tokio::test]
async fn test_pinless_dashboard_refuses_unsetting_a_security_key() {
    let dir = dir("unset");
    let (router, _) = dashboard(&dir, None);
    let (status, reply) = post_settings(
        &router,
        json!({"MENTISDB_WEBHOOK_ALLOWED_CIDRS": ""}),
        Auth::Anonymous,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{reply}");
}

/// An ordinary setting still saves on a PIN-less dashboard.
#[tokio::test]
async fn test_pinless_dashboard_still_saves_an_ordinary_setting() {
    let dir = dir("ordinary");
    let (router, _) = dashboard(&dir, None);
    let (status, reply) = post_settings(
        &router,
        json!({"MENTISDB_DEDUP_SCAN_WINDOW": "48"}),
        Auth::Anonymous,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(env_file_mentions(&dir, "MENTISDB_DEDUP_SCAN_WINDOW"));
}

/// A PIN-less dashboard can still set a PIN: that is how an operator
/// bootstraps protection. The new PIN is saved for the next start.
#[tokio::test]
async fn test_pinless_dashboard_can_bootstrap_a_pin() {
    let dir = dir("bootstrap");
    let (router, _) = dashboard(&dir, None);
    let (status, reply) = post_settings(
        &router,
        json!({"MENTISDB_DASHBOARD_PIN": "bootstrap-9731"}),
        Auth::Anonymous,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(env_file_mentions(&dir, "MENTISDB_DASHBOARD_PIN"));
    std::env::remove_var("MENTISDB_DASHBOARD_PIN");
}

/// With a PIN and a signed-in session a security key is saved.
#[tokio::test]
async fn test_pin_and_session_may_change_a_security_key() {
    let dir = dir("session");
    let (router, sessions) = dashboard(&dir, Some("2468"));
    sessions
        .lock()
        .unwrap()
        .insert("tok-1".to_string(), std::time::Instant::now());
    let (status, reply) = post_settings(
        &router,
        json!({"MENTISDB_WEBHOOK_ALLOWED_CIDRS": "10.1.0.0/16"}),
        Auth::Session("tok-1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(env_file_mentions(&dir, "MENTISDB_WEBHOOK_ALLOWED_CIDRS"));
    std::env::remove_var("MENTISDB_WEBHOOK_ALLOWED_CIDRS");
}

/// With a PIN configured but no session (or a forged one) the request never
/// reaches the handler and nothing is saved: the PIN existing is not enough.
#[tokio::test]
async fn test_pin_without_a_session_is_refused() {
    let dir = dir("nosession");
    let (router, _) = dashboard(&dir, Some("2468"));
    let body = json!({"MENTISDB_WEBHOOK_ALLOWED_CIDRS": "10.1.0.0/16"});
    for auth in [Auth::Anonymous, Auth::Session("forged")] {
        let (status, _) = post_settings(&router, body.clone(), auth).await;
        assert!(
            status == StatusCode::SEE_OTHER || status == StatusCode::FORBIDDEN,
            "{status}"
        );
    }
    assert!(!env_file_mentions(&dir, "MENTISDB_WEBHOOK_ALLOWED_CIDRS"));
}

/// A request that mixes an ordinary key with a security key is refused whole:
/// the ordinary key is not saved either.
#[tokio::test]
async fn test_mixed_request_is_refused_whole() {
    let dir = dir("mixed");
    let (router, _) = dashboard(&dir, None);
    let (status, reply) = post_settings(
        &router,
        json!({
            "MENTISDB_DEDUP_SCAN_WINDOW": "48",
            "MENTISDB_DASHBOARD_ALLOWED_HOSTS": "evil.example",
        }),
        Auth::Anonymous,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{reply}");
    assert_eq!(reply["locked"], json!(["MENTISDB_DASHBOARD_ALLOWED_HOSTS"]));
    assert!(!dir.join(".env").exists(), "nothing may be written");
}

/// GET reports security keys as locked on a PIN-less dashboard and unlocked
/// with a PIN, so the Settings screen can disable and explain them.
#[tokio::test]
async fn test_get_settings_reports_locked_only_without_a_pin() {
    let dir = dir("locked");
    let (pinless, _) = dashboard(&dir, None);
    let rows = get_settings(&pinless, Auth::Anonymous).await;
    for row in rows.as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let expect = SECURITY_KEYS.iter().any(|(key, _)| *key == name);
        assert_eq!(row["locked"], json!(expect), "{name}");
    }
    let (with_pin, sessions) = dashboard(&dir, Some("2468"));
    sessions
        .lock()
        .unwrap()
        .insert("tok-2".to_string(), std::time::Instant::now());
    let rows = get_settings(&with_pin, Auth::Session("tok-2")).await;
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["locked"] == json!(false)));
}
