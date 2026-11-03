#![cfg(feature = "server")]
//! The `my.mentisdb.com` alias is an own host on TLS listeners only.
//!
//! The alias is a public DNS name MentisDB does not control; whoever controls
//! it could rebind it to `127.0.0.1`. Plain-HTTP listeners therefore refuse it
//! unless `MENTISDB_DASHBOARD_ALLOWED_HOSTS` lists it, while the HTTPS
//! listeners (the name the certificate carries) keep accepting it. These tests
//! start real listeners: an HTTP one and an HTTPS one with a generated
//! certificate, and call them over the wire with the alias as `Host`.
//!
//! The host variables are process global, so every test takes [`ENV_LOCK`].

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{
    ensure_tls_cert, rest_router, start_https_mcp_server, start_https_rest_server,
    start_mcp_server, start_rest_server, ListenerTls, MentisDbServiceConfig,
};
use mentisdb::StorageAdapterKind;
use std::net::SocketAddr;
use tower::util::ServiceExt;

const ALLOWED_HOSTS_ENV: &str = "MENTISDB_DASHBOARD_ALLOWED_HOSTS";
const BIND_HOST_ENV: &str = "MENTISDB_BIND_HOST";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn config(dir: &std::path::Path) -> MentisDbServiceConfig {
    MentisDbServiceConfig::new(dir.to_path_buf(), "default", StorageAdapterKind::Binary)
}

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}

/// A client that reaches `127.0.0.1:port` as `my.mentisdb.com`, accepting the
/// generated self-signed certificate.
fn client_for(port: u16) -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .resolve("my.mentisdb.com", SocketAddr::from(([127, 0, 0, 1], port)))
        .build()
        .unwrap()
}

async fn mcp_status(client: &reqwest::Client, url: &str) -> StatusCode {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let response = client
        .post(url)
        .header("accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .unwrap();
    StatusCode::from_u16(response.status().as_u16()).unwrap()
}

async fn rest_status(client: &reqwest::Client, url: &str, browser: bool) -> StatusCode {
    let mut request = client.get(format!("{url}/v1/chains"));
    if browser {
        request = request.header("sec-fetch-site", "same-origin");
    }
    let response = request.send().await.unwrap();
    StatusCode::from_u16(response.status().as_u16()).unwrap()
}

/// Over HTTP the alias is refused; over HTTPS it is accepted, on MCP and REST.
#[tokio::test]
async fn the_alias_is_refused_over_http_and_accepted_over_https() {
    let _lock = ENV_LOCK.lock().await;
    std::env::remove_var(ALLOWED_HOSTS_ENV);
    std::env::remove_var(BIND_HOST_ENV);
    // The daemon installs rustls' process-wide crypto provider at startup.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    ensure_tls_cert(&cert, &key).unwrap();

    let (http_mcp, _b1) = start_mcp_server(loopback(), config(dir.path()))
        .await
        .unwrap();
    let http_rest = start_rest_server(loopback(), config(dir.path()))
        .await
        .unwrap();
    let (https_mcp, _b2) =
        start_https_mcp_server(loopback(), config(dir.path()), cert.clone(), key.clone())
            .await
            .unwrap();
    let https_rest =
        start_https_rest_server(loopback(), config(dir.path()), cert.clone(), key.clone())
            .await
            .unwrap();

    let c = client_for(http_mcp.local_addr().port());
    assert_eq!(
        mcp_status(
            &c,
            &format!("http://my.mentisdb.com:{}/", http_mcp.local_addr().port())
        )
        .await,
        StatusCode::FORBIDDEN,
        "alias over plain MCP"
    );
    let c = client_for(http_rest.local_addr().port());
    let url = format!("http://my.mentisdb.com:{}", http_rest.local_addr().port());
    assert_eq!(
        rest_status(&c, &url, true).await,
        StatusCode::FORBIDDEN,
        "alias over plain REST, from a browser"
    );
    // REST on a loopback bind checks Host for every client, not only browsers.
    assert_eq!(rest_status(&c, &url, false).await, StatusCode::FORBIDDEN);

    let c = client_for(https_mcp.local_addr().port());
    assert_eq!(
        mcp_status(
            &c,
            &format!("https://my.mentisdb.com:{}/", https_mcp.local_addr().port())
        )
        .await,
        StatusCode::OK,
        "alias over HTTPS MCP"
    );
    let c = client_for(https_rest.local_addr().port());
    let url = format!("https://my.mentisdb.com:{}", https_rest.local_addr().port());
    assert_eq!(
        rest_status(&c, &url, true).await,
        StatusCode::OK,
        "alias over HTTPS REST, from a browser"
    );
    // A foreign name is refused on TLS too.
    let evil = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .resolve("evil.example.com", https_rest.local_addr())
        .build()
        .unwrap();
    assert_eq!(
        rest_status(
            &evil,
            &format!(
                "https://evil.example.com:{}",
                https_rest.local_addr().port()
            ),
            true
        )
        .await,
        StatusCode::FORBIDDEN
    );

    for mut handle in [http_mcp, http_rest, https_mcp, https_rest] {
        let _ = handle.shutdown();
    }
}

/// Listing the alias in `MENTISDB_DASHBOARD_ALLOWED_HOSTS` restores it on a
/// plain listener: the operator chose to trust the name.
#[tokio::test]
async fn listing_the_alias_accepts_it_over_http() {
    let _lock = ENV_LOCK.lock().await;
    std::env::remove_var(BIND_HOST_ENV);
    std::env::set_var(ALLOWED_HOSTS_ENV, "my.mentisdb.com");
    let dir = tempfile::tempdir().unwrap();
    let (mut http_mcp, _b) = start_mcp_server(loopback(), config(dir.path()))
        .await
        .unwrap();
    let port = http_mcp.local_addr().port();
    let c = client_for(port);
    let status = mcp_status(&c, &format!("http://my.mentisdb.com:{port}/")).await;
    std::env::remove_var(ALLOWED_HOSTS_ENV);
    assert_eq!(status, StatusCode::OK);
    let _ = http_mcp.shutdown();
}

/// The marker a listener adds is what the browser guard reads: an embedder
/// that terminates TLS itself can add `ListenerTls(true)`; without it the
/// router judges the connection as plain HTTP.
#[tokio::test]
async fn the_listener_marker_decides_the_alias_for_an_embedded_router() {
    let _lock = ENV_LOCK.lock().await;
    std::env::remove_var(ALLOWED_HOSTS_ENV);
    std::env::remove_var(BIND_HOST_ENV);
    let dir = tempfile::tempdir().unwrap();
    let router = rest_router(config(dir.path()));
    let request = |tls: Option<bool>| {
        let mut request = Request::get("/v1/chains")
            .header("host", "my.mentisdb.com:9474")
            .header("origin", "https://my.mentisdb.com:9474")
            .body(Body::empty())
            .unwrap();
        if let Some(tls) = tls {
            request.extensions_mut().insert(ListenerTls(tls));
        }
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::from((
                [127, 0, 0, 1],
                44000,
            ))));
        request
    };
    for (marker, expected) in [
        (None, StatusCode::FORBIDDEN),
        (Some(false), StatusCode::FORBIDDEN),
        (Some(true), StatusCode::OK),
    ] {
        let status = router
            .clone()
            .oneshot(request(marker))
            .await
            .unwrap()
            .status();
        assert_eq!(status, expected, "marker {marker:?}");
    }
}
