#![cfg(feature = "server")]
//! Large request bodies: a client may send a body far over 1 MiB with a read
//! proof (declaring the digest it signed) or a bearer token, without the server
//! buffering it just to hash it, and without weakening the proof.
//!
//! * a proof with `X-Mentis-Content-Sha256` is verified before the body is read
//!   and the body is checked against that digest as it streams: the signed body
//!   goes through, any other body fails and changes nothing;
//! * a proof without it (every existing client) behaves exactly as before and
//!   stays limited to 1 MiB, with an error that says how to send more;
//! * `MENTISDB_MAX_BODY_BYTES` (`with_max_body_bytes`) caps everything with `413`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::read_auth::{
    body_digest, content_digest_header_value, fresh_nonce, sign_request, CONTENT_DIGEST_HEADER,
    LEGACY_SIGNED_BODY_BYTES, READ_AUTH_HEADER,
};
use mentisdb::server::{rest_router, MentisDbServiceConfig, MIN_MAX_BODY_BYTES};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use tower::util::ServiceExt;

const SEED: [u8; 32] = [21; 32];
const URI: &str = "/v1/import-markdown";

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_bigbody_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    MentisDbServiceConfig::new(dir, "big", StorageAdapterKind::Binary)
}

/// Bytes of non-entry text (ignored by the importer) that make a body large
/// without making the import slow: 80 KiB per unit.
const UNIT: usize = 80 * 1024;

/// A MEMORY.md with 5 thoughts and `units * 80 KiB` of other text.
fn markdown(units: usize) -> String {
    let entries: String = (0..5)
        .map(|n| format!("- [{n}] Insight: imported entry {n}\n"))
        .collect();
    let filler: String = (0..units)
        .map(|_| format!("{}\n", "lorem ipsum ".repeat(UNIT / 12)))
        .collect();
    entries + &filler
}

fn import_body(units: usize) -> Vec<u8> {
    json!({"chain_key": "big", "markdown": markdown(units), "default_agent_id": "importer"})
        .to_string()
        .into_bytes()
}

fn proof(body: &[u8]) -> String {
    sign_request(
        &SEED,
        "importer",
        "importer-1",
        "POST",
        URI,
        body,
        chrono::Utc::now().timestamp(),
        &fresh_nonce(),
    )
}

/// `POST /v1/import-markdown` with the given headers; `(status, json)`.
async fn send(
    router: &axum::Router,
    body: Vec<u8>,
    headers: &[(&str, String)],
) -> (StatusCode, Value) {
    let mut builder = Request::post(URI).header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    let mut request = builder.body(Body::from(body)).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            46000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())),
    )
}

/// Thoughts on the chain, through the REST API.
async fn thought_count(router: &axum::Router) -> usize {
    let response = router
        .clone()
        .oneshot(
            Request::post("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(json!({"chain_key": "big"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    if response.status() != StatusCode::OK {
        return 0;
    }
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    json["thoughts"].as_array().map_or(0, Vec::len)
}

fn signed_headers(body: &[u8]) -> Vec<(&'static str, String)> {
    vec![
        (READ_AUTH_HEADER, proof(body)),
        (
            CONTENT_DIGEST_HEADER,
            content_digest_header_value(&body_digest(body)),
        ),
    ]
}

/// A 3 MiB import with a read proof and its digest header is accepted and every
/// entry lands.
#[tokio::test]
async fn a_large_import_with_a_digest_header_succeeds() {
    let router = rest_router(config("digest_ok"));
    let body = import_body(40);
    assert!(body.len() > 3 * LEGACY_SIGNED_BODY_BYTES, "{}", body.len());
    let (status, reply) = send(&router, body.clone(), &signed_headers(&body)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["count"], 5);
    assert_eq!(thought_count(&router).await, 5);
}

/// A body that is not the one the proof signed, even by one byte and with the
/// right length and the right declared digest, is refused and nothing is
/// imported.
#[tokio::test]
async fn a_tampered_body_is_rejected_and_changes_nothing() {
    let router = rest_router(config("tamper"));
    let body = import_body(40);
    let headers = signed_headers(&body);
    let mut tampered = body.clone();
    let middle = tampered.len() / 2;
    tampered[middle] = if tampered[middle] == b'x' { b'y' } else { b'x' };
    let (status, reply) = send(&router, tampered, &headers).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(reply.to_string().contains("digest"), "{reply}");
    assert_eq!(thought_count(&router).await, 0, "nothing was imported");
    // The genuine body still goes through afterwards (the first one never
    // spent the proof: it did not get as far as the handler).
    let (status, _) = send(&router, body, &headers).await;
    assert_eq!(status, StatusCode::OK);
}

/// A digest header that is not the signed body's digest fails the proof itself.
#[tokio::test]
async fn a_lying_digest_header_fails_the_proof() {
    let router = rest_router(config("lying"));
    let body = import_body(1);
    let other = import_body(2);
    let headers = vec![
        (READ_AUTH_HEADER, proof(&body)),
        (
            CONTENT_DIGEST_HEADER,
            content_digest_header_value(&body_digest(&other)),
        ),
    ];
    let (status, reply) = send(&router, other, &headers).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{reply}");
    assert_eq!(thought_count(&router).await, 0);
}

/// A malformed or repeated digest header is a 400, not a silent fallback.
#[tokio::test]
async fn a_malformed_digest_header_is_refused() {
    let router = rest_router(config("malformed"));
    let body = import_body(1);
    for bad in [
        "",
        "abc",
        &"AB".repeat(32),
        &format!("sha256={}", "ab".repeat(32)),
    ] {
        let headers = vec![
            (READ_AUTH_HEADER, proof(&body)),
            (CONTENT_DIGEST_HEADER, bad.to_owned()),
        ];
        let (status, reply) = send(&router, body.clone(), &headers).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {reply}");
    }
    let mut request = Request::post(URI)
        .header(READ_AUTH_HEADER, proof(&body))
        .header(CONTENT_DIGEST_HEADER, "ab".repeat(32))
        .header(CONTENT_DIGEST_HEADER, "cd".repeat(32))
        .body(Body::from(body))
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            46001,
        ))));
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// An old-style proof (no digest header) keeps working unchanged for a body up
/// to 1 MiB, and is told how to send more beyond it.
#[tokio::test]
async fn an_old_style_proof_still_works_and_is_capped_at_one_mib() {
    let router = rest_router(config("legacy"));
    let small = import_body(5);
    assert!(small.len() < LEGACY_SIGNED_BODY_BYTES);
    let (status, reply) = send(&router, small.clone(), &[(READ_AUTH_HEADER, proof(&small))]).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["count"], 5);

    let big = import_body(40);
    let (status, reply) = send(&router, big.clone(), &[(READ_AUTH_HEADER, proof(&big))]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    assert!(
        reply.to_string().contains("X-Mentis-Content-Sha256"),
        "the error says how to send a larger body: {reply}"
    );
    assert_eq!(thought_count(&router).await, 5);
}

/// A body over the configured cap is refused with 413, with or without a
/// declared `Content-Length`, on the read-proof path and with no auth at all.
#[tokio::test]
async fn a_body_over_the_cap_is_413() {
    let router = rest_router(config("cap").with_max_body_bytes(MIN_MAX_BODY_BYTES));
    let body = import_body(20); // about 1.6 MiB
    assert!(body.len() > MIN_MAX_BODY_BYTES);
    // Declared up front: refused before anything is read.
    let mut headers = signed_headers(&body);
    headers.push(("content-length", body.len().to_string()));
    let (status, reply) = send(&router, body.clone(), &headers).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    // Not declared: refused while it streams.
    let (status, reply) = send(&router, body.clone(), &signed_headers(&body)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    // No proof at all.
    let (status, reply) = send(&router, body, &[]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    assert_eq!(thought_count(&router).await, 0);
}

/// A bearer router with one global token; `(router, Authorization header)`.
fn bearer_router(name: &str, cap: Option<usize>) -> (axum::Router, Vec<(&'static str, String)>) {
    let mut config = config(name).with_bearer_token_access(true);
    if let Some(cap) = cap {
        config = config.with_max_body_bytes(cap);
    }
    let token = config
        .bearer_token_store
        .create("importer", BearerTokenScope::Global)
        .unwrap()
        .token;
    (
        rest_router(config),
        vec![("authorization", format!("Bearer {token}"))],
    )
}

/// A bearer token can send a body well over 1 MiB (the authorization step used
/// to buffer at most 1 MiB and answer 413 beyond it).
#[tokio::test]
async fn a_bearer_token_can_send_a_large_body() {
    let (router, auth) = bearer_router("bearer", None);
    let (status, reply) = send(&router, import_body(40), &auth).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["count"], 5);
}

/// The configured cap applies to the bearer path as well.
#[tokio::test]
async fn a_bearer_body_over_the_cap_is_413() {
    let (router, auth) = bearer_router("bearer_cap", Some(MIN_MAX_BODY_BYTES));
    let (status, reply) = send(&router, import_body(20), &auth).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{reply}");
    assert_eq!(thought_count(&router).await, 0);
}

/// A bearer token and a digest-declaring read proof together: both checks
/// apply to the same large body.
#[tokio::test]
async fn bearer_and_a_digest_proof_together() {
    let (router, mut headers) = bearer_router("both", None);
    let body = import_body(40);
    headers.extend(signed_headers(&body));
    let (status, reply) = send(&router, body, &headers).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let mut tampered = import_body(40);
    let at = tampered.len() / 3;
    tampered[at] ^= 1;
    let (status, _) = send(&router, tampered, &headers).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------------------ the client

/// `mentisdb memory import` with a read key sends a file well over 1 MiB to a
/// real listening daemon and every entry lands: the CLI declares the digest the
/// proof signs, so the daemon does not have to buffer the body to check it.
#[tokio::test(flavor = "multi_thread")]
async fn the_cli_imports_a_large_file_with_a_read_key() {
    let router = rest_router(config("cli"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("mentisdb_cli_big_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("MEMORY.md");
    std::fs::write(&file, markdown(40)).unwrap();
    assert!(std::fs::metadata(&file).unwrap().len() > 3 * LEGACY_SIGNED_BODY_BYTES as u64);
    let seed = dir.join("read.key");
    std::fs::write(&seed, format!("{}\n", "07".repeat(32))).unwrap();

    let args: Vec<String> = [
        "memory",
        "import",
        file.to_str().unwrap(),
        "--chain",
        "big",
        "--read-key-file",
        seed.to_str().unwrap(),
        "--read-agent",
        "importer",
        "--read-key-id",
        "importer-1",
        "--url",
        &url,
        "--json",
    ]
    .map(String::from)
    .to_vec();
    let (out, code) = tokio::task::spawn_blocking(move || {
        let argv = std::iter::once("mentisdb".to_string()).chain(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = mentisdb::cli::run_with_io(argv, &mut std::io::empty(), &mut out, &mut err);
        (
            String::from_utf8(out).unwrap() + &String::from_utf8(err).unwrap(),
            code,
        )
    })
    .await
    .unwrap();
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "{out}"
    );
    assert!(out.contains("\"count\":5"), "{out}");
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------- the setting

/// `MENTISDB_MAX_BODY_BYTES` is read by the daemon's configuration: 64 MiB when
/// unset or unparsable, the value when valid, never below 1 MiB.
#[test]
fn the_cap_comes_from_the_environment() {
    use mentisdb::server::{MentisDbServerConfig, DEFAULT_MAX_BODY_BYTES};
    // The only test in this binary that touches the process environment.
    let read = |value: Option<&str>| {
        match value {
            Some(value) => std::env::set_var("MENTISDB_MAX_BODY_BYTES", value),
            None => std::env::remove_var("MENTISDB_MAX_BODY_BYTES"),
        }
        MentisDbServerConfig::from_env().service.max_body_bytes
    };
    assert_eq!(read(None), DEFAULT_MAX_BODY_BYTES);
    assert_eq!(read(Some("not a number")), DEFAULT_MAX_BODY_BYTES);
    assert_eq!(read(Some("5242880")), 5 * 1024 * 1024);
    assert_eq!(read(Some(" 2097152 ")), 2 * 1024 * 1024);
    assert_eq!(read(Some("10")), MIN_MAX_BODY_BYTES);
    std::env::remove_var("MENTISDB_MAX_BODY_BYTES");
}
