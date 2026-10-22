#![cfg(feature = "server")]

//! Dashboard DNS-rebinding defence (Host allowlist), login CSRF coverage and
//! per-IP login rate limiting.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::ConnectInfo,
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
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

fn router(pin: Option<&str>) -> (axum::Router, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_host_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let app = dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::<String, Arc<RwLock<MentisDb>>>::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    });
    (app, dir)
}

async fn send(app: &axum::Router, mut req: Request<Body>, ip: [u8; 4]) -> StatusCode {
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((ip, 40000))));
    app.clone().oneshot(req).await.unwrap().status()
}

fn get(uri: &str, host: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("host", host)
        .body(Body::empty())
        .unwrap()
}

fn login(host: &str, pin: &str, csrf: bool) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/dashboard/login")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded");
    if csrf {
        b = b.header("x-mentisdb-dashboard", "1");
    }
    b.body(Body::from(format!("pin={pin}"))).unwrap()
}

/// All env-dependent assertions live in one test (process env is global).
#[tokio::test]
async fn host_allowlist_blocks_rebinding_on_every_route() {
    std::env::remove_var("MENTISDB_BIND_HOST");
    std::env::remove_var(dashboard_impl::DASHBOARD_ALLOWED_HOSTS_ENV);
    let (app, dir) = router(None);

    for host in [
        "localhost:9475",
        "127.0.0.1:9475",
        "[::1]:9475",
        "LOCALHOST",
    ] {
        for uri in ["/dashboard", "/dashboard/api/version", "/dashboard/login"] {
            let status = send(&app, get(uri, host), [127, 0, 0, 1]).await;
            assert_eq!(status, StatusCode::OK, "{host} {uri}");
        }
    }
    // Rebinding attacker hostname: rejected on GET, API and login page.
    for uri in ["/dashboard", "/dashboard/api/version", "/dashboard/login"] {
        let status = send(&app, get(uri, "evil.example.com:9475"), [127, 0, 0, 1]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    // Rejected for the login POST even with the CSRF header.
    let status = send(&app, login("evil.example.com", "x", true), [127, 0, 0, 1]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Configured public hostname is accepted once allowed.
    std::env::set_var(
        dashboard_impl::DASHBOARD_ALLOWED_HOSTS_ENV,
        "mem.example.org, other.test",
    );
    assert_eq!(
        send(
            &app,
            get("/dashboard", "mem.example.org:9475"),
            [127, 0, 0, 1]
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        send(&app, get("/dashboard", "evil.example.com"), [127, 0, 0, 1]).await,
        StatusCode::FORBIDDEN
    );
    std::env::remove_var(dashboard_impl::DASHBOARD_ALLOWED_HOSTS_ENV);

    // Non-wildcard bind host names that host only.
    std::env::set_var("MENTISDB_BIND_HOST", "192.168.7.7");
    assert!(dashboard_impl::dashboard_host_allowed("192.168.7.7:9475"));
    assert!(!dashboard_impl::dashboard_host_allowed("192.168.7.8:9475"));
    // Wildcard bind allows own interface IPs but not arbitrary ones.
    std::env::set_var("MENTISDB_BIND_HOST", "0.0.0.0");
    assert!(dashboard_impl::dashboard_host_allowed("127.0.0.1"));
    assert!(!dashboard_impl::dashboard_host_allowed("203.0.113.77"));
    assert!(!dashboard_impl::dashboard_host_allowed("evil.example.com"));
    std::env::remove_var("MENTISDB_BIND_HOST");

    let _ = std::fs::remove_dir_all(dir);
}

/// The login POST now needs the same-origin / CSRF opt-in.
#[tokio::test]
async fn login_post_requires_origin_or_csrf_header() {
    let (app, dir) = router(Some("1234"));
    let host = "localhost:9475";
    assert_eq!(
        send(&app, login(host, "1234", false), [127, 0, 0, 2]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&app, login(host, "1234", true), [127, 0, 0, 2]).await,
        StatusCode::SEE_OTHER
    );
    // Cross-site Origin is rejected even with the header.
    let mut req = login(host, "1234", true);
    req.headers_mut()
        .insert("origin", "http://evil.example.com".parse().unwrap());
    assert_eq!(send(&app, req, [127, 0, 0, 2]).await, StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(dir);
}

/// One client's failed attempts must not lock out another client IP.
#[tokio::test]
async fn login_rate_limit_is_per_client_ip() {
    let (app, dir) = router(Some("right-pin"));
    let host = "localhost:9475";
    let attacker = [10, 9, 9, 9];
    for _ in 0..dashboard_impl::RATE_LIMIT_MAX_ATTEMPTS {
        assert_eq!(
            send(&app, login(host, "wrong", true), attacker).await,
            StatusCode::SEE_OTHER
        );
    }
    assert_eq!(
        send(&app, login(host, "right-pin", true), attacker).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // The owner, from a different address, can still log in.
    assert_eq!(
        send(&app, login(host, "right-pin", true), [10, 9, 9, 10]).await,
        StatusCode::SEE_OTHER
    );
    let _ = std::fs::remove_dir_all(dir);
}
