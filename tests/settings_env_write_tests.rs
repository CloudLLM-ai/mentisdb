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

/// Parse `<dir>/.env` the way the daemon does at startup and return every
/// assignment in file order. Fails the test on the first line the loader
/// rejects: that is the failure that silently drops all later settings.
fn parse_env_file(dir: &Path) -> Vec<(String, String)> {
    dotenvy::from_path_iter(dir.join(".env"))
        .expect("open .env")
        .map(|item| item.expect("every line of the saved .env must parse"))
        .collect()
}

/// Values that are not safe to write raw: each used to corrupt the file or
/// load as something else (`$` interpolation, ` #` comments, a space or quote
/// that aborts the parse, a trailing backslash).
const HOSTILE_VALUES: &[&str] = &[
    "B=x y",
    "\"",
    "a \" b",
    "it's",
    "pa$word#1",
    "${HOME}/x",
    "$HOME",
    "trailing space ",
    "x # not a comment",
    "C:\\dir\\",
    "back\\slash",
    "'single'",
    "tab\tinside",
    "ünï-cødé",
    "a=b=c",
    "#leading-hash",
];

/// Whatever the value, the saved file must parse, give the value back
/// unchanged, and keep the lines after it (the PIN here) loadable. Raw
/// `NAME=value` writing failed on most of these.
#[tokio::test]
async fn test_saved_value_round_trips_through_the_env_loader() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::fs::write(
        dir.join(".env"),
        "MENTISDB_DASHBOARD_PIN=keep-me\nMENTISDB_BEARER_TOKEN_ACCESS=true\n",
    )
    .unwrap();

    for value in HOSTILE_VALUES {
        let (status, body) = save(&dir, json!({ "MENTISDB_UPDATE_REPO": value })).await;
        assert_eq!(status, StatusCode::OK, "{value:?}: {body}");

        let parsed = parse_env_file(&dir);
        let lookup = |name: &str| {
            parsed
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(lookup("MENTISDB_UPDATE_REPO"), Some(*value), "{value:?}");
        assert_eq!(lookup("MENTISDB_DASHBOARD_PIN"), Some("keep-me"));
        assert_eq!(lookup("MENTISDB_BEARER_TOKEN_ACCESS"), Some("true"));
    }
    std::env::remove_var("MENTISDB_UPDATE_REPO");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PIN containing `$` and `#` must survive a save unchanged: unquoted it
/// loaded as a different, shorter PIN and the operator was locked out.
#[tokio::test]
async fn test_pin_with_shell_metacharacters_loads_back_identically() {
    let _env = env_guard().await;
    let dir = unique_dir();
    let (status, body) = save(&dir, json!({ "MENTISDB_DASHBOARD_PIN": "pa$word#1" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_env_file(&dir);
    assert_eq!(
        parsed,
        vec![(
            "MENTISDB_DASHBOARD_PIN".to_string(),
            "pa$word#1".to_string()
        )]
    );
    std::env::remove_var("MENTISDB_DASHBOARD_PIN");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A rotated value must replace an `export NAME=` line (the loader reads the
/// first occurrence, so an appended duplicate was ignored) and delete later
/// duplicates; `NAME =` spacing matches too.
#[tokio::test]
async fn test_save_updates_export_and_spaced_lines_and_drops_duplicates() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::fs::write(
        dir.join(".env"),
        "# mine\nexport MENTISDB_UPDATE_REPO=old/one\nOTHER=1\nMENTISDB_UPDATE_REPO=old/two\nMENTISDB_LOG_FILE = /tmp/x.log\n",
    )
    .unwrap();

    let (status, body) = save(
        &dir,
        json!({ "MENTISDB_UPDATE_REPO": "new/repo", "MENTISDB_LOG_FILE": "/tmp/y.log" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let text = std::fs::read_to_string(dir.join(".env")).unwrap();
    assert_eq!(
        text,
        "# mine\nexport MENTISDB_UPDATE_REPO=new/repo\nOTHER=1\nMENTISDB_LOG_FILE=/tmp/y.log\n"
    );
    std::env::remove_var("MENTISDB_UPDATE_REPO");
    std::env::remove_var("MENTISDB_LOG_FILE");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An existing `.env` that cannot be read must stop the save: the old code
/// ignored the error, rewrote the file from nothing and so deleted every
/// setting the request did not mention. The file and the process environment
/// stay as they were.
#[tokio::test]
async fn test_unreadable_env_file_aborts_the_save_and_is_left_alone() {
    let _env = env_guard().await;
    let dir = unique_dir();
    let original = b"MENTISDB_DASHBOARD_PIN=secret\n\xff\xfe not utf-8\n";
    std::fs::write(dir.join(".env"), original).unwrap();
    std::env::remove_var("MENTISDB_UPDATE_REPO");

    let (status, _) = save(&dir, json!({ "MENTISDB_UPDATE_REPO": "x/y" })).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(std::fs::read(dir.join(".env")).unwrap(), original);
    assert!(std::env::var("MENTISDB_UPDATE_REPO").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The file is replaced through a temporary file and a rename, always
/// owner-only, and no temporary file is left behind.
#[cfg(unix)]
#[tokio::test]
async fn test_save_is_owner_only_and_leaves_no_temp_files() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_guard().await;
    let dir = unique_dir();
    let env_path = dir.join(".env");
    std::fs::write(&env_path, "KEEP=1\n").unwrap();
    std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let inode_before = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&env_path).unwrap());

    let (status, body) = save(&dir, json!({ "MENTISDB_UPDATE_REPO": "x/y" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let meta = std::fs::metadata(&env_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&meta),
        inode_before,
        "an in-place truncate keeps the inode; an atomic replace does not"
    );
    let stray: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".env.") || n.ends_with(".tmp"))
        .collect();
    assert!(stray.is_empty(), "temporary files left behind: {stray:?}");
    assert!(std::fs::read_to_string(&env_path)
        .unwrap()
        .contains("KEEP=1"));
    std::env::remove_var("MENTISDB_UPDATE_REPO");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Settings that are checked by kind or by a startup validator, with values
/// the next start would reject or silently misread: `(name, bad value)`.
const BAD_VALUES: &[(&str, &str)] = &[
    ("MENTISDB_MAX_BODY_BYTES", "lots"),
    ("MENTISDB_MAX_BODY_BYTES", "10"),
    ("MENTISDB_MAX_BODY_BYTES", "2000000000"),
    ("MENTISDB_MAX_INFLIGHT_BODY_BYTES", "0"),
    ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "0"),
    ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "99999"),
    ("MENTISDB_MIN_BODY_BYTES_PER_SEC", "-1"),
    ("MENTISDB_WEBHOOK_ALLOWED_CIDRS", "10.0.0.0/33"),
    ("MENTISDB_WEBHOOK_ALLOWED_CIDRS", "not-a-cidr"),
    ("MENTISDB_WEBHOOK_ALLOWED_CIDRS", "0.0.0.0/0"),
    ("MENTISDB_MCP_PORT", "99999"),
    ("MENTISDB_REST_PORT", "abc"),
    ("MENTISDB_HTTPS_MCP_PORT", "-1"),
    ("MENTISDB_HTTPS_REST_PORT", "9474 "),
    ("MENTISDB_DASHBOARD_PORT", "65536"),
    ("MENTISDB_GROUP_COMMIT_MS", "fast"),
    ("MENTISDB_FSYNC", "maybe"),
    ("MENTISDB_AUTO_FLUSH", "2"),
];

/// A value the daemon would refuse or misread is rejected with 400 and a
/// message naming the setting, and neither the process environment nor the
/// `.env` file changes. Before this check the save went through and the next
/// start failed, with the dashboard unreachable to repair it.
#[tokio::test]
async fn test_bad_values_are_rejected_by_kind_and_range() {
    let _env = env_guard().await;
    let dir = unique_dir();
    for (name, value) in BAD_VALUES {
        std::env::remove_var(name);
        let (status, body) = save(&dir, json!({ *name: *value })).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}={value:?}: {body}");
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains(name), "{name}={value:?}: {message}");
        assert!(std::env::var(name).is_err(), "{name}={value:?} was applied");
        assert!(!dir.join(".env").exists(), "{name}={value:?} was written");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Validation covers the whole request before anything is applied: one bad
/// entry keeps the good ones out of the environment and the file.
#[tokio::test]
async fn test_one_bad_setting_rejects_the_whole_request() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::env::remove_var("MENTISDB_UPDATE_REPO");

    let (status, _) = save(
        &dir,
        json!({ "MENTISDB_UPDATE_REPO": "x/y", "MENTISDB_REST_PORT": "nope" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(std::env::var("MENTISDB_UPDATE_REPO").is_err());
    assert!(!dir.join(".env").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Values the daemon accepts still save: the check must not be stricter than
/// startup (`0` disables the HTTPS and dashboard ports; CIDR lists may use
/// commas and blanks; booleans may be `yes`).
#[tokio::test]
async fn test_valid_values_are_saved() {
    let _env = env_guard().await;
    let dir = unique_dir();
    let good = [
        ("MENTISDB_MAX_BODY_BYTES", "67108864"),
        ("MENTISDB_MAX_INFLIGHT_BODY_BYTES", "1"),
        ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "3600"),
        ("MENTISDB_MIN_BODY_BYTES_PER_SEC", "0"),
        (
            "MENTISDB_WEBHOOK_ALLOWED_CIDRS",
            "100.64.0.0/10, 10.8.0.0/24",
        ),
        ("MENTISDB_DASHBOARD_PORT", "0"),
        ("MENTISDB_HTTPS_REST_PORT", "65535"),
        ("MENTISDB_AUTO_EDGE_THRESHOLD", "0.85"),
        ("MENTISDB_FSYNC", "yes"),
    ];
    for (name, _) in &good {
        std::env::remove_var(name);
    }
    let settings: serde_json::Map<String, Value> = good
        .iter()
        .map(|(n, v)| (n.to_string(), json!(v)))
        .collect();

    let (status, body) = save(&dir, Value::Object(settings)).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_env_file(&dir);
    for (name, value) in &good {
        assert!(
            parsed.iter().any(|(n, v)| n == name && v == value),
            "{name} missing from {parsed:?}"
        );
        std::env::remove_var(name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cleared field (and Reset) posts an empty string. That means "unset": the
/// variable leaves the environment and its line leaves the file. Writing
/// `NAME=` instead made the next start fail on the body limits and the
/// webhook CIDR list. A stale blank line from the old behaviour is removed
/// even though the variable was already unset.
#[tokio::test]
async fn test_empty_value_unsets_the_variable_and_removes_its_line() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::fs::write(
        dir.join(".env"),
        "MENTISDB_GROUP_COMMIT_MS=5\nMENTISDB_MAX_BODY_BYTES=\nKEEP=1\n",
    )
    .unwrap();
    std::env::set_var("MENTISDB_GROUP_COMMIT_MS", "5");
    std::env::remove_var("MENTISDB_MAX_BODY_BYTES");

    let (status, body) = save(
        &dir,
        json!({ "MENTISDB_GROUP_COMMIT_MS": "", "MENTISDB_MAX_BODY_BYTES": "" }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(std::env::var("MENTISDB_GROUP_COMMIT_MS").is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join(".env")).unwrap(),
        "KEEP=1\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PIN the daemon would read back as "no PIN" must be refused. Startup
/// trims `MENTISDB_DASHBOARD_PIN` and treats an empty result as unset, so a
/// whitespace-only PIN saved from the dashboard used to restart as an open
/// dashboard, and one with a stray leading or trailing blank would load as
/// something other than what the operator typed. The request fails with 400,
/// the PIN is not applied, and the file is not written. An empty value still
/// means "unchanged" and a PIN with inner spaces is fine.
#[tokio::test]
async fn test_blank_or_padded_pin_is_rejected() {
    let _env = env_guard().await;
    let dir = unique_dir();
    std::env::remove_var("MENTISDB_DASHBOARD_PIN");
    for pin in ["   ", "\t", " 1234", "1234 ", "\u{a0}", "\u{3000}12"] {
        let (status, body) = save(&dir, json!({ "MENTISDB_DASHBOARD_PIN": pin })).await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{pin:?}: {body}");
        let message = body["message"].as_str().unwrap_or_default();
        assert!(message.contains("MENTISDB_DASHBOARD_PIN"), "{message}");
        assert!(message.contains("whitespace"), "{message}");
        assert!(std::env::var("MENTISDB_DASHBOARD_PIN").is_err(), "{pin:?}");
        assert!(!dir.join(".env").exists(), "{pin:?} was written");
    }

    let (status, body) = save(&dir, json!({ "MENTISDB_DASHBOARD_PIN": "" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!dir.join(".env").exists());

    let (status, body) = save(&dir, json!({ "MENTISDB_DASHBOARD_PIN": "12 34" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        parse_env_file(&dir),
        [("MENTISDB_DASHBOARD_PIN".into(), "12 34".into())]
    );
    std::env::remove_var("MENTISDB_DASHBOARD_PIN");
    let _ = std::fs::remove_dir_all(&dir);
}
