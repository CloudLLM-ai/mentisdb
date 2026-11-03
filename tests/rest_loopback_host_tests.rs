#![cfg(feature = "server")]
//! REST on a loopback bind checks `Host` for every request.
//!
//! `cloudllm_mcp` 0.4 already turns away a foreign `Host` on a loopback MCP
//! bind, for every client. REST now does the same through the shared host
//! policy: a `curl`-like client (no `Origin`, no `Sec-Fetch-Site`) naming a
//! foreign host gets `403`, as does a browser. Wildcard binds keep the
//! browser-only guard.
//!
//! `MENTISDB_BIND_HOST` and `MENTISDB_DASHBOARD_ALLOWED_HOSTS` are process
//! global, so every test takes [`ENV_LOCK`].

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mentisdb::server::{
    ensure_tls_cert, rest_router_on, start_https_rest_server, start_rest_server, ListenerTls,
    MentisDbServiceConfig,
};
use mentisdb::StorageAdapterKind;
use serde_json::Value;
use std::net::SocketAddr;
use tower::util::ServiceExt;

const ALLOWED_HOSTS_ENV: &str = "MENTISDB_DASHBOARD_ALLOWED_HOSTS";
const BIND_HOST_ENV: &str = "MENTISDB_BIND_HOST";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn config(dir: &std::path::Path) -> MentisDbServiceConfig {
    MentisDbServiceConfig::new(dir.to_path_buf(), "default", StorageAdapterKind::Binary)
}

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 9472))
}

fn wildcard() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 9472))
}

async fn send(
    router: &Router,
    method: &str,
    host: Option<&str>,
    extra: &[(&str, &str)],
    tls: bool,
) -> (StatusCode, Value) {
    let (uri, body) = if method == "POST" {
        (
            "/v1/bootstrap",
            Body::from(r#"{"chain_key":"c","content":"x"}"#),
        )
    } else {
        ("/v1/chains", Body::empty())
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(body).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            44000,
        ))));
    if tls {
        request.extensions_mut().insert(ListenerTls(true));
    }
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

fn clear_env() {
    std::env::remove_var(ALLOWED_HOSTS_ENV);
    std::env::remove_var(BIND_HOST_ENV);
}

/// A non-browser client naming a foreign host is refused on GET and POST, with
/// the dashboard's message.
#[tokio::test]
async fn a_loopback_bind_refuses_a_foreign_host_from_a_non_browser_client() {
    let _lock = ENV_LOCK.lock().await;
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let router = rest_router_on(config(dir.path()), loopback());
    for method in ["GET", "POST"] {
        for host in [
            "evil.example",
            "evil.example:9472",
            "127.0.0.1.evil.example",
            "localhost.evil.example",
            "203.0.113.7:9472",
        ] {
            let (status, body) = send(&router, method, Some(host), &[], false).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {host}: {body}");
            let message = body["error"].as_str().unwrap_or_default();
            assert!(
                message.contains(&format!("host '{host}' is not allowed")),
                "{message}"
            );
            assert!(
                message.contains("MENTISDB_DASHBOARD_ALLOWED_HOSTS"),
                "{message}"
            );
        }
    }
}

/// The loopback names, an allowlisted name and a bind-host name are accepted;
/// a request with no `Host` at all passes.
#[tokio::test]
async fn a_loopback_bind_accepts_its_own_names() {
    let _lock = ENV_LOCK.lock().await;
    clear_env();
    std::env::set_var(ALLOWED_HOSTS_ENV, "mem.proxy.example, 203.0.113.7");
    std::env::set_var(BIND_HOST_ENV, "named.example.net");
    let dir = tempfile::tempdir().unwrap();
    let router = rest_router_on(config(dir.path()), loopback());
    let mut statuses = Vec::new();
    for host in [
        "localhost",
        "localhost:9472",
        "127.0.0.1:9472",
        "[::1]:9472",
        "MEM.proxy.example",
        "203.0.113.7:9472",
        "named.example.net",
    ] {
        statuses.push((host, send(&router, "GET", Some(host), &[], false).await.0));
    }
    let (no_host, _) = send(&router, "GET", None, &[], false).await;
    clear_env();
    for (host, status) in statuses {
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    assert_eq!(no_host, StatusCode::OK, "no Host header at all");
}

/// The `my.mentisdb.com` alias follows the TLS-only rule: refused on a plain
/// listener (unless listed), accepted when the listener marks the request TLS.
#[tokio::test]
async fn the_alias_follows_the_tls_only_rule_on_a_loopback_rest_bind() {
    let _lock = ENV_LOCK.lock().await;
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let router = rest_router_on(config(dir.path()), loopback());
    let alias = Some("my.mentisdb.com:9474");
    assert_eq!(
        send(&router, "GET", alias, &[], false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&router, "GET", alias, &[], true).await.0,
        StatusCode::OK
    );
    std::env::set_var(ALLOWED_HOSTS_ENV, "my.mentisdb.com");
    let listed = send(&router, "GET", alias, &[], false).await.0;
    clear_env();
    assert_eq!(listed, StatusCode::OK, "listed alias on plain HTTP");
}

/// Wildcard binds are unchanged: no `Host` check for programmatic clients, the
/// browser guard for browsers.
#[tokio::test]
async fn a_wildcard_bind_is_unchanged() {
    let _lock = ENV_LOCK.lock().await;
    clear_env();
    let dir = tempfile::tempdir().unwrap();
    let router = rest_router_on(config(dir.path()), wildcard());
    for host in ["evil.example", "mentisdb:9472", "192.168.1.50:9472"] {
        assert_eq!(
            send(&router, "GET", Some(host), &[], false).await.0,
            StatusCode::OK,
            "programmatic client, {host}"
        );
    }
    assert_eq!(
        send(
            &router,
            "GET",
            Some("evil.example"),
            &[("sec-fetch-site", "same-origin")],
            false
        )
        .await
        .0,
        StatusCode::FORBIDDEN,
        "a browser still needs an allowed host"
    );
}

/// Over real listeners: HTTP loopback refuses a foreign name and the alias;
/// HTTPS loopback accepts the alias and still refuses a foreign name, both
/// from a plain (non-browser) client.
#[tokio::test]
async fn real_loopback_listeners_apply_the_host_check() {
    let _lock = ENV_LOCK.lock().await;
    clear_env();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    ensure_tls_cert(&cert, &key).unwrap();
    let any_port = SocketAddr::from(([127, 0, 0, 1], 0));
    let mut http = start_rest_server(any_port, config(dir.path()))
        .await
        .unwrap();
    let mut https =
        start_https_rest_server(any_port, config(dir.path()), cert.clone(), key.clone())
            .await
            .unwrap();

    let get = |name: &'static str, addr: SocketAddr, scheme: &'static str| async move {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .resolve(name, addr)
            .build()
            .unwrap();
        client
            .get(format!("{scheme}://{name}:{}/v1/chains", addr.port()))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    };
    let (h, s) = (http.local_addr(), https.local_addr());
    assert_eq!(get("localhost", h, "http").await, 200);
    assert_eq!(get("evil.example", h, "http").await, 403);
    assert_eq!(get("my.mentisdb.com", h, "http").await, 403);
    assert_eq!(get("my.mentisdb.com", s, "https").await, 200);
    assert_eq!(get("evil.example", s, "https").await, 403);
    let _ = http.shutdown();
    let _ = https.shutdown();
}
