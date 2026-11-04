#![cfg(feature = "server")]
//! Who may make the daemon hold how much request body, and for how long.
//!
//! * every route but the import family is held to 2 MiB;
//! * a caller without a valid bearer token never gets the daemon to read its
//!   body: it is refused (`401`, or `413` for a declared length over the small
//!   cap) before any of the body is pulled off the connection;
//! * the body bytes held by requests in flight are budgeted (`503` beyond it);
//! * a body that stops arriving times out;
//! * the cap holds for chunked bodies that declare no length.

use axum::body::Body;
use axum::body::Bytes;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::read_auth::{
    body_digest, content_digest_header_value, fresh_nonce, sign_request, CONTENT_DIGEST_HEADER,
    READ_AUTH_HEADER,
};
use mentisdb::server::{rest_router, MentisDbServiceConfig, MIN_MAX_BODY_BYTES};

use mentisdb::StorageAdapterKind;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt;
use tower::util::ServiceExt;

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_admit_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    MentisDbServiceConfig::new(dir, "adm", StorageAdapterKind::Binary)
}

/// A JSON body of about `bytes` bytes for a route that takes `markdown`/`text`.
fn padded(bytes: usize) -> Vec<u8> {
    json!({"chain_key": "adm", "markdown": "x".repeat(bytes)})
        .to_string()
        .into_bytes()
}

fn request(uri: &str, body: Body, headers: &[(&str, String)]) -> Request<Body> {
    let mut builder = Request::post(uri).header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let mut request = builder.body(body).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            47000,
        ))));
    request
}

async fn status(router: &axum::Router, request: Request<Body>) -> StatusCode {
    router.clone().oneshot(request).await.unwrap().status()
}

/// A body of `chunks` chunks of `chunk` bytes that counts how many were pulled.
fn counted(chunks: usize, chunk: usize, polled: &Arc<AtomicUsize>) -> Body {
    let polled = Arc::clone(polled);
    Body::from_stream(tokio_stream::iter((0..chunks).map(move |_| {
        polled.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; chunk]))
    })))
}

/// A body that sends `first` and then nothing, forever.
fn stalled(first: &'static [u8]) -> Body {
    Body::from_stream(
        tokio_stream::once(Ok::<_, std::io::Error>(Bytes::from_static(first)))
            .chain(tokio_stream::pending()),
    )
}

/// Only the import family may carry more than 2 MiB; a 3 MiB body to any other
/// route is `413` (it used to be accepted when the limit was raised everywhere).
#[tokio::test]
async fn a_non_import_route_rejects_a_3_mib_body_and_an_import_route_takes_it() {
    let router = rest_router(config("routes"));
    let big = padded(3 * 1024 * 1024);
    let other = request("/v1/ranked-search", Body::from(big.clone()), &[]);
    assert_eq!(status(&router, other).await, StatusCode::PAYLOAD_TOO_LARGE);
    let import = request("/v1/import-markdown", Body::from(big.clone()), &[]);
    assert_eq!(status(&router, import).await, StatusCode::OK);
    // Declared lengths are refused up front, on the same rule.
    let declared = request(
        "/v1/ranked-search",
        Body::from(big),
        &[("content-length", (3 * 1024 * 1024 + 40).to_string())],
    );
    assert_eq!(
        status(&router, declared).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

fn bearer_router(name: &str) -> (axum::Router, String) {
    let config = config(name).with_bearer_token_access(true);
    let token = config
        .bearer_token_store
        .create("importer", BearerTokenScope::Global)
        .unwrap()
        .token;
    (rest_router(config), token)
}

/// With bearer enforcement on, a request with no (or a wrong) token is refused
/// from its headers: `401` for a body of any size without a declared length,
/// `413` for a declared length over the small cap, and in neither case is the
/// body pulled off the connection.
#[tokio::test]
async fn an_unauthenticated_large_post_is_refused_without_reading_the_body() {
    let (router, _token) = bearer_router("anon");
    for headers in [vec![], vec![("authorization", "Bearer wrong".to_owned())]] {
        // 10 MiB, no declared length: refused before the first chunk is pulled.
        let polled = Arc::new(AtomicUsize::new(0));
        let body = counted(160, 64 * 1024, &polled);
        let response = request("/v1/import-markdown", body, &headers);
        assert_eq!(status(&router, response).await, StatusCode::UNAUTHORIZED);
        assert_eq!(polled.load(Ordering::SeqCst), 0, "{headers:?}");
        // 10 MiB declared: refused at once.
        let polled = Arc::new(AtomicUsize::new(0));
        let mut declared = headers.clone();
        declared.push(("content-length", (10 * 1024 * 1024).to_string()));
        let response = request(
            "/v1/import-markdown",
            counted(160, 64 * 1024, &polled),
            &declared,
        );
        assert_eq!(
            status(&router, response).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(polled.load(Ordering::SeqCst), 0, "{headers:?}");
    }
}

/// A valid token does get the large cap on an import route.
#[tokio::test]
async fn an_authenticated_caller_gets_the_large_cap() {
    let (router, token) = bearer_router("authed");
    let auth = [("authorization", format!("Bearer {token}"))];
    let ok = request(
        "/v1/import-markdown",
        Body::from(padded(3 * 1024 * 1024)),
        &auth,
    );
    assert_eq!(status(&router, ok).await, StatusCode::OK);
    let other = request(
        "/v1/ranked-search",
        Body::from(padded(3 * 1024 * 1024)),
        &auth,
    );
    assert_eq!(status(&router, other).await, StatusCode::PAYLOAD_TOO_LARGE);
}

/// A body over the cap that declares no length (chunked, HTTP/2) is cut off with
/// `413` as it streams, on the read-proof path as well.
#[tokio::test]
async fn a_chunked_body_over_the_cap_is_413() {
    let router = rest_router(config("chunked").with_max_body_bytes(MIN_MAX_BODY_BYTES));
    // 1.5 MiB in 16 KiB chunks, no Content-Length.
    let polled = Arc::new(AtomicUsize::new(0));
    let plain = request("/v1/import-markdown", counted(96, 16 * 1024, &polled), &[]);
    assert_eq!(status(&router, plain).await, StatusCode::PAYLOAD_TOO_LARGE);
    // The same with a read proof that declares its digest: the digest-checked
    // body is also capped.
    let body = vec![b'x'; 96 * 16 * 1024];
    let uri = "/v1/import-markdown";
    let headers = [
        (
            READ_AUTH_HEADER,
            sign_request(
                &[9; 32],
                "a",
                "k",
                "POST",
                uri,
                &body,
                chrono::Utc::now().timestamp(),
                &fresh_nonce(),
            ),
        ),
        (
            CONTENT_DIGEST_HEADER,
            content_digest_header_value(&body_digest(&body)),
        ),
    ];
    let polled = Arc::new(AtomicUsize::new(0));
    let proof = request(uri, counted(96, 16 * 1024, &polled), &headers);
    assert_eq!(status(&router, proof).await, StatusCode::PAYLOAD_TOO_LARGE);
}

/// A body that starts and then stops arriving is cut off at the idle timeout
/// instead of holding its connection (and reservation) forever.
#[tokio::test]
async fn a_slow_drip_body_times_out() {
    let router = rest_router(config("drip").with_body_idle_timeout(Duration::from_millis(200)));
    let response = request("/v1/import-markdown", stalled(b"{\"chain_key\":"), &[]);
    let outcome = tokio::time::timeout(Duration::from_secs(10), router.clone().oneshot(response))
        .await
        .expect("a stalled body must not hang the request");
    assert_eq!(outcome.unwrap().status(), StatusCode::REQUEST_TIMEOUT);
}

/// The body bytes in flight are budgeted: while one 1 MiB upload is held open,
/// a second one beyond the budget gets `503` with `Retry-After`, and it is
/// admitted once the first finishes.
#[tokio::test(flavor = "current_thread")]
async fn bodies_beyond_the_in_flight_budget_get_503() {
    let router = rest_router(
        config("budget")
            .with_max_body_bytes(MIN_MAX_BODY_BYTES)
            .with_max_inflight_body_bytes(MIN_MAX_BODY_BYTES + MIN_MAX_BODY_BYTES / 2),
    );
    let declared = [("content-length", (MIN_MAX_BODY_BYTES - 100).to_string())];
    let first = tokio::spawn({
        let router = router.clone();
        let first = request(
            "/v1/import-markdown",
            stalled(b"{\"chain_key\":"),
            &declared,
        );
        async move { router.oneshot(first).await.unwrap().status() }
    });
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let second = request("/v1/import-markdown", Body::empty(), &declared);
    let response = router.clone().oneshot(second).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key("retry-after"));
    // The first upload goes away; its share is free again.
    first.abort();
    let _ = first.await;
    let third = request("/v1/import-markdown", Body::empty(), &declared);
    assert_ne!(
        status(&router, third).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

/// A small request (an append) from the same peer is never starved by imports:
/// two maximum-size imports are held open, a third is refused (the peer's share
/// of the large pool), and an append from the same address still succeeds.
#[tokio::test(flavor = "current_thread")]
async fn imports_in_flight_never_starve_small_requests() {
    let router = rest_router(config("nostarve").with_max_body_bytes(MIN_MAX_BODY_BYTES));
    let declared = [("content-length", MIN_MAX_BODY_BYTES.to_string())];
    let holders: Vec<_> = (0..2)
        .map(|_| {
            let router = router.clone();
            let held = request(
                "/v1/import-markdown",
                stalled(b"{\"chain_key\":"),
                &declared,
            );
            tokio::spawn(async move { router.oneshot(held).await.unwrap().status() })
        })
        .collect();
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    let third = request("/v1/import-markdown", Body::empty(), &declared);
    assert_eq!(
        status(&router, third).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let append = json!({"chain_key": "adm", "agent_id": "a", "thought_type": "Insight", "content": "still works"});
    let small = request("/v1/thoughts", Body::from(append.to_string()), &[]);
    assert_eq!(status(&router, small).await, StatusCode::OK);
    for holder in holders {
        holder.abort();
    }
}

/// Slow-drip bodies that merely *declare* a large length cost nothing: with
/// bearer-token access on, anonymous requests declaring 1 MiB on an ordinary
/// route are not charged for it, and a request with a valid token still goes
/// through.
#[tokio::test(flavor = "current_thread")]
async fn declared_lengths_of_anonymous_requests_are_not_charged() {
    let (router, token) = {
        let config = config("anon_drip")
            .with_bearer_token_access(true)
            .with_max_body_bytes(MIN_MAX_BODY_BYTES);
        let token = config
            .bearer_token_store
            .create("t", BearerTokenScope::Global)
            .unwrap()
            .token;
        (rest_router(config), token)
    };
    let declared = [("content-length", (MIN_MAX_BODY_BYTES - 1).to_string())];
    let drips: Vec<_> = (0..200)
        .map(|_| {
            let router = router.clone();
            let drip = request("/v1/head", stalled(b"{"), &declared);
            tokio::spawn(async move { router.oneshot(drip).await.unwrap().status() })
        })
        .collect();
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    let auth = [("authorization", format!("Bearer {token}"))];
    let body = json!({"chain_key": "adm"}).to_string();
    let ok = request("/v1/head", Body::from(body), &auth);
    assert_eq!(status(&router, ok).await, StatusCode::OK);
    for drip in drips {
        drip.abort();
    }
}

/// A body that stalls reaches the caller as `408`, and one that exhausts the
/// in-flight budget as `503` with `Retry-After`, also through the bearer
/// middleware (which used to report both as a generic `400`).
#[tokio::test]
async fn stalled_and_over_budget_bodies_get_408_and_503_with_a_bearer_token() {
    let config = config("stops")
        .with_bearer_token_access(true)
        .with_max_body_bytes(MIN_MAX_BODY_BYTES)
        .with_max_inflight_body_bytes(MIN_MAX_BODY_BYTES)
        .with_body_idle_timeout(Duration::from_millis(200));
    let token = config
        .bearer_token_store
        .create("t", BearerTokenScope::Global)
        .unwrap()
        .token;
    let router = rest_router(config);
    let auth = [("authorization", format!("Bearer {token}"))];
    let stall = request("/v1/head", stalled(b"{\"chain_key\":"), &auth);
    assert_eq!(status(&router, stall).await, StatusCode::REQUEST_TIMEOUT);

    // Two 600 KiB chunks on an import route: the pool holds one maximum body
    // (1 MiB), so the second chunk exhausts it.
    let chunks = tokio_stream::iter(
        (0..2).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 600 * 1024]))),
    );
    let big = request("/v1/import-markdown", Body::from_stream(chunks), &auth);
    let response = router.clone().oneshot(big).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key("retry-after"));
}

/// With bearer-token access off no middleware reads the body, so a handler's own
/// extractor sees the failure: a body that exhausts the in-flight budget is still
/// `503` with `Retry-After` (not the extractor's generic `400`).
#[tokio::test]
async fn over_budget_bodies_are_503_without_bearer_tokens_too() {
    let router = rest_router(
        config("nobearer503")
            .with_max_body_bytes(MIN_MAX_BODY_BYTES)
            .with_max_inflight_body_bytes(MIN_MAX_BODY_BYTES),
    );
    let chunks = tokio_stream::iter(
        (0..2).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 600 * 1024]))),
    );
    let big = request("/v1/import-markdown", Body::from_stream(chunks), &[]);
    let response = router.clone().oneshot(big).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key("retry-after"));
}

/// A zero (or tiny) in-flight budget does not refuse every import: it is
/// raised to one maximum body.
#[tokio::test]
async fn a_zero_in_flight_budget_is_raised_to_one_maximum_body() {
    let router = rest_router(
        config("zero")
            .with_max_body_bytes(MIN_MAX_BODY_BYTES)
            .with_max_inflight_body_bytes(0),
    );
    let body = json!({"chain_key": "adm", "markdown": "- [0] Insight: fits"}).to_string();
    let import = request("/v1/import-markdown", Body::from(body), &[]);
    assert_eq!(status(&router, import).await, StatusCode::OK);
}

/// The environment: a bad value is an error the daemon refuses to start on.
#[test]
fn invalid_body_limit_environment_is_rejected() {
    // The only test in this binary that touches these variables.
    let check = |name: &str, value: &str| {
        std::env::set_var(name, value);
        let result = mentisdb::server::validate_body_limits_env();
        std::env::remove_var(name);
        result
    };
    assert!(mentisdb::server::validate_body_limits_env().is_ok());
    for (name, bad) in [
        ("MENTISDB_MAX_BODY_BYTES", "64MB"),
        ("MENTISDB_MAX_BODY_BYTES", ""),
        ("MENTISDB_MAX_BODY_BYTES", "10"),
        ("MENTISDB_MAX_BODY_BYTES", "1073741825"),
        ("MENTISDB_MAX_BODY_BYTES", "-1"),
        ("MENTISDB_MAX_INFLIGHT_BODY_BYTES", "0"),
        ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "0"),
        ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "soon"),
    ] {
        let error = check(name, bad).expect_err(&format!("{name}={bad:?}"));
        assert!(error.contains(name), "{error}");
    }
    for (name, good) in [
        ("MENTISDB_MAX_BODY_BYTES", "1048576"),
        ("MENTISDB_MAX_BODY_BYTES", "1073741824"),
        ("MENTISDB_MAX_INFLIGHT_BODY_BYTES", "1"),
        ("MENTISDB_BODY_IDLE_TIMEOUT_SECS", "3600"),
    ] {
        assert!(check(name, good).is_ok(), "{name}={good}");
    }
}

/// The daemon itself exits at startup, naming the variable, on a bad value.
#[test]
fn the_daemon_refuses_to_start_on_a_bad_body_limit() {
    let dir = std::env::temp_dir().join(format!("mentisdb_badlimit_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mentisdb"))
        .arg("--headless")
        .env("MENTISDB_DIR", &dir)
        .env("MENTISDB_MAX_BODY_BYTES", "lots")
        .env("MENTISDB_MCP_PORT", "0")
        .env("MENTISDB_REST_PORT", "0")
        .env("MENTISDB_HTTPS_MCP_PORT", "0")
        .env("MENTISDB_HTTPS_REST_PORT", "0")
        .env("MENTISDB_DASHBOARD_PORT", "0")
        .env("MENTISDB_UPDATE_CHECK", "false")
        .env("MENTISDB_STARTUP_SOUND", "false")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("MENTISDB_MAX_BODY_BYTES"), "{text}");
}
