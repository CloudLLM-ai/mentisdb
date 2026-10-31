//! DNS-rebinding and cross-site browser requests must not reach the REST or MCP
//! listeners while bearer enforcement is off, and programmatic clients must be
//! unaffected.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::server::{combined_mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use std::net::{IpAddr, Ipv4Addr};
use tower::util::ServiceExt;

fn routers() -> (axum::Router, axum::Router, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_rebind_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let config = MentisDbServiceConfig::new(dir.clone(), "main", StorageAdapterKind::Binary);
    (
        rest_router(config.clone()),
        combined_mcp_router(config, IpAddr::V4(Ipv4Addr::LOCALHOST)),
        dir,
    )
}

async fn status(
    app: &axum::Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> StatusCode {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    app.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn rebinding_page_is_refused_on_rest_and_mcp() {
    let (rest, mcp, dir) = routers();
    for app in [&rest, &mcp] {
        // Same-origin from the browser's point of view: foreign Host + Sec-Fetch-Site.
        let rebound = status(
            app,
            "GET",
            "/health",
            &[
                ("host", "evil.example:9472"),
                ("sec-fetch-site", "same-origin"),
            ],
        )
        .await;
        assert_eq!(rebound, StatusCode::FORBIDDEN);
        // Cross-site POST from a foreign origin, loopback Host.
        let cross = status(
            app,
            "GET",
            "/health",
            &[
                ("host", "127.0.0.1:9472"),
                ("origin", "https://evil.example"),
            ],
        )
        .await;
        assert_eq!(cross, StatusCode::FORBIDDEN);
        let null_origin = status(
            app,
            "GET",
            "/health",
            &[("host", "127.0.0.1:9472"), ("origin", "null")],
        )
        .await;
        assert_eq!(null_origin, StatusCode::FORBIDDEN);
        let userinfo = status(
            app,
            "GET",
            "/health",
            &[
                ("host", "127.0.0.1:9472"),
                ("origin", "http://localhost@evil.example"),
            ],
        )
        .await;
        assert_eq!(userinfo, StatusCode::FORBIDDEN);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn programmatic_and_local_browser_requests_still_work() {
    let (rest, mcp, dir) = routers();
    for app in [&rest, &mcp] {
        // No Origin, no Sec-Fetch-*: curl, the CLI, MCP agents; any Host.
        assert_eq!(
            status(app, "GET", "/health", &[("host", "mentisdb:9472")]).await,
            StatusCode::OK
        );
        // A browser on the loopback origin.
        assert_eq!(
            status(
                app,
                "GET",
                "/health",
                &[
                    ("host", "localhost:9472"),
                    ("origin", "http://localhost:9472"),
                    ("sec-fetch-site", "same-origin"),
                ],
            )
            .await,
            StatusCode::OK
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
