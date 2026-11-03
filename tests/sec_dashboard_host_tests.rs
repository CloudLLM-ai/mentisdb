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
    // The dashboard is only ever served over TLS; the listener marks it so.
    req.extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
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
    assert!(dashboard_impl::dashboard_host_allowed(
        "192.168.7.7:9475",
        true
    ));
    assert!(!dashboard_impl::dashboard_host_allowed(
        "192.168.7.8:9475",
        true
    ));
    // Wildcard bind allows own interface IPs but not arbitrary ones.
    std::env::set_var("MENTISDB_BIND_HOST", "0.0.0.0");
    assert!(dashboard_impl::dashboard_host_allowed("127.0.0.1", true));
    assert!(!dashboard_impl::dashboard_host_allowed(
        "203.0.113.77",
        true
    ));
    assert!(!dashboard_impl::dashboard_host_allowed(
        "evil.example.com",
        true
    ));
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

/// The static list handed to `cloudllm_mcp` and the per-request decision the
/// dashboard and the browser guard take are one policy: for every sample
/// `Host`, the list contains the host exactly when `allows` accepts it, on
/// plain and on TLS listeners. Built from explicit values, so it does not
/// touch the process environment.
#[test]
fn library_host_list_and_dashboard_decision_agree() {
    use dashboard_impl::HostPolicy;
    let hosts = [
        "localhost",
        "127.0.0.1",
        "[::1]",
        "my.mentisdb.com",
        "my.mentisdb.com.evil.com",
        "evil.example.com",
        "203.0.113.7",
        "203.0.113.8",
        "vps.example.org",
        "[2001:db8::7]",
        "[2001:db8::8]",
        "[2001:db8::1]",
        "mem.example.net",
        "192.168.7.7",
        "192.168.7.8",
        "10.0.0.9",
        "0.0.0.0",
    ];
    for tls in [false, true] {
        let policies = [
            HostPolicy::new(vec![], None, tls),
            HostPolicy::new(
                vec!["203.0.113.7".into(), " Vps.Example.org ".into()],
                None,
                tls,
            ),
            HostPolicy::new(vec!["2001:db8::7".into(), "".into()], None, tls),
            HostPolicy::new(vec![], Some("mem.example.net".into()), tls),
            HostPolicy::new(vec![], Some("192.168.7.7".into()), tls),
            HostPolicy::new(vec!["10.0.0.9".into()], Some("2001:db8::1".into()), tls),
            HostPolicy::new(vec![], Some("  ".into()), tls),
            HostPolicy::new(vec!["My.MentisDB.com".into()], None, tls),
        ];
        for policy in &policies {
            let listed = policy.library_hosts();
            for host in hosts {
                for authority in [host.to_string(), format!("{host}:9471")] {
                    let in_list = listed.iter().any(|entry| entry == host);
                    assert_eq!(
                        policy.allows(&authority),
                        in_list,
                        "{authority} under {policy:?}; list {listed:?}"
                    );
                }
            }
            assert!(!policy.allows(""), "an empty Host names nothing");
        }
    }
}

/// Regression: `my.mentisdb.com` was accepted on every listener, so whoever
/// controls that public DNS name could rebind it to `127.0.0.1` and pass the
/// Host and Origin guards of a plain-HTTP listener. It is now an own host on
/// TLS listeners (the certificate name) and on plain ones only when the
/// operator lists it in `MENTISDB_DASHBOARD_ALLOWED_HOSTS`.
#[test]
fn the_alias_is_a_trust_anchor_on_tls_listeners_only() {
    use dashboard_impl::HostPolicy;
    let alias = [
        "my.mentisdb.com",
        "MY.MentisDB.com:9473",
        "my.mentisdb.com.",
    ];
    let plain = HostPolicy::new(vec![], None, false);
    let tls = HostPolicy::new(vec![], None, true);
    let listed = HostPolicy::new(vec!["my.mentisdb.com".into()], None, false);
    for host in alias {
        assert!(!plain.allows(host), "plain listener accepted {host}");
        assert!(tls.allows(host), "TLS listener refused {host}");
        assert!(listed.allows(host), "listed alias refused on plain {host}");
    }
    assert!(!plain.library_hosts().iter().any(|h| h == "my.mentisdb.com"));
    assert!(tls.library_hosts().iter().any(|h| h == "my.mentisdb.com"));
    assert!(listed
        .library_hosts()
        .iter()
        .any(|h| h == "my.mentisdb.com"));
    // Look-alikes never pass, with or without TLS.
    for host in [
        "my.mentisdb.com.evil.com",
        "xmy.mentisdb.com",
        "mentisdb.com",
    ] {
        assert!(!tls.allows(host), "{host}");
    }
}

/// A dashboard request that did not come through a TLS listener (no
/// `ListenerTls` marker) is judged as plain HTTP: the alias is refused, the
/// loopback names still work.
#[tokio::test]
async fn an_unmarked_dashboard_request_is_treated_as_plain_http() {
    // Leaves the process environment alone: the host-allowlist test in this
    // binary owns it, and neither of its values names the alias.
    let (app, dir) = router(None);
    let mut req = get("/dashboard/api/version", "my.mentisdb.com:9475");
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let mut req = get("/dashboard/api/version", "localhost:9475");
    req.extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    assert_eq!(app.oneshot(req).await.unwrap().status(), StatusCode::OK);
    let _ = std::fs::remove_dir_all(dir);
}

/// A wildcard `MENTISDB_BIND_HOST` lists the machine's interface IPs for the
/// library, and `allows` accepts exactly those, never an unrelated address.
#[test]
fn wildcard_bind_lists_interface_ips() {
    let policy = dashboard_impl::HostPolicy::new(vec![], Some("0.0.0.0".into()), false);
    let listed = policy.library_hosts();
    for iface in if_addrs::get_if_addrs().unwrap() {
        let ip = iface.addr.ip();
        let host = match ip {
            std::net::IpAddr::V4(v4) => v4.to_string(),
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        };
        assert!(listed.contains(&host), "{host} missing from {listed:?}");
        assert!(policy.allows(&host), "{host}");
    }
    assert!(!policy.allows("203.0.113.77"));
    assert!(!listed.iter().any(|h| h == "0.0.0.0"));
}
