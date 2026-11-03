#![cfg(feature = "server")]
//! Holes found in `cloudllm_mcp` 0.3.6 (unauthenticated SSE, prefix-match
//! Origin check, empty bearer token, CR/LF in SSE fields) and the proof that
//! MentisDB stays closed to each of them on `cloudllm_mcp` 0.4, which fixes
//! them in the library. MentisDB used to work around the first, second and
//! fourth with its own event-stream route and its own check; those
//! workarounds are gone, and these tests now exercise the library's code.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{
    combined_mcp_router, rest_router, standard_mcp_router, MentisDbServiceConfig,
};
use mentisdb::StorageAdapterKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower::util::ServiceExt;

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!("mentisdb_mcplib_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary)
}

async fn call(router: &Router, mut request: Request<Body>) -> (StatusCode, String) {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            43000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn rpc_list(extra: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::post("/")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    builder
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))
        .unwrap()
}

/// (a) With no broadcaster `GET /` is not an event stream at all; with one and
/// bearer enforcement on, subscribing needs a token (the library authorizes the
/// stream as action `events/subscribe`).
#[tokio::test]
async fn the_event_stream_is_not_open_to_anonymous_subscribers() {
    let get = || {
        Request::get("/")
            .header("accept", "text/event-stream")
            .body(Body::empty())
            .unwrap()
    };
    let router = combined_mcp_router(config("sse_none"), "127.0.0.1".parse().unwrap());
    let (status, body) = call(&router, get()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{body}");

    let config = config("sse_bearer").with_bearer_token_access(true);
    let token = config
        .bearer_token_store
        .create("sub", BearerTokenScope::Global)
        .unwrap()
        .token;
    let (router, _broadcaster) = standard_mcp_router(config, IpAddr::V4(Ipv4Addr::LOCALHOST));
    let (status, _) = call(&router, get()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut with_token = get();
    with_token
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    // (the stream never ends, so only the status line is read)
    with_token
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            43002,
        ))));
    let response = router.clone().oneshot(with_token).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// (b) A browser page on `localhost.evil.com` must not reach the server.
/// MentisDB's guard compares the `Origin` host exactly against its host policy
/// (the library's own Origin check is switched off so the two cannot disagree).
#[tokio::test]
async fn lookalike_origins_are_refused() {
    let (router, _b) = standard_mcp_router(config("origin"), IpAddr::V4(Ipv4Addr::LOCALHOST));
    for origin in [
        "http://localhost.evil.com",
        "http://127.0.0.1.evil.com",
        "https://localhost.evil.com:8443",
        "http://localhost@evil.com",
    ] {
        let (status, body) = call(
            &router,
            rpc_list(&[("origin", origin), ("host", "127.0.0.1:9471")]),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{origin}: {body}");
    }
    let (status, body) = call(
        &router,
        rpc_list(&[
            ("origin", "http://localhost:3000"),
            ("host", "127.0.0.1:9471"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// (c) An empty bearer token authenticates nobody, on REST and on MCP.
#[tokio::test]
async fn an_empty_bearer_token_is_rejected() {
    let config = config("empty_bearer").with_bearer_token_access(true);
    config
        .bearer_token_store
        .create("real", BearerTokenScope::Global)
        .unwrap();
    let rest = rest_router(config.clone());
    for value in ["Bearer ", "Bearer", "bearer x"] {
        let (status, _) = call(
            &rest,
            Request::get("/v1/chains")
                .header("authorization", value)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "REST {value:?}");
    }
    let (mcp, _b) = standard_mcp_router(config, IpAddr::V4(Ipv4Addr::LOCALHOST));
    let (status, _) = call(&mcp, rpc_list(&[("authorization", "Bearer ")])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// (d) A CR or LF in an event name or id cannot forge extra SSE fields or
/// events; the library strips them before it writes the frame.
#[tokio::test]
async fn crlf_in_event_fields_cannot_forge_events() {
    let (router, broadcaster) =
        standard_mcp_router(config("crlf"), IpAddr::V4(Ipv4Addr::LOCALHOST));
    let mut request = Request::get("/")
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            43001,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    broadcaster.send(mcp::SseMessage::with_event(
        "tool_called\nevent: forged\ndata: {\"pwned\":true}\n\n",
        serde_json::json!({"ok": true}),
    ));
    let mut message = mcp::SseMessage::data(serde_json::json!({"n": 1}));
    message.id = Some("1\r\nevent: forged".into());
    broadcaster.send(message);
    let mut body = response.into_body();
    let mut seen = String::new();
    while let Ok(Some(Ok(frame))) = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        http_body_util_next(&mut body),
    )
    .await
    {
        seen.push_str(&String::from_utf8_lossy(&frame));
        if seen.contains("\"n\":1") {
            break;
        }
    }
    // The injected text stayed inside the field it was smuggled into: no line
    // starts a forged event, and each frame has exactly the lines it should.
    let lines: Vec<&str> = seen.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        lines
            .iter()
            .all(|l| ["event:", "id:", "data:"].iter().any(|p| l.starts_with(p))),
        "{seen:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("event: forged") || l.starts_with("data: {\"pwned")),
        "{seen:?}"
    );
    assert_eq!(lines.len(), 4, "{seen:?}");
}

async fn http_body_util_next(body: &mut Body) -> Option<Result<axum::body::Bytes, axum::Error>> {
    use std::future::poll_fn;
    use std::pin::Pin;
    poll_fn(|cx| {
        use axum::body::HttpBody;
        Pin::new(&mut *body).poll_frame(cx)
    })
    .await
    .map(|result| result.map(|frame| frame.into_data().unwrap_or_default()))
}
