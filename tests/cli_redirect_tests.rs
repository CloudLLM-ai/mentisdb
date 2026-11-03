#![cfg(feature = "server")]
//! The CLI never follows a redirect.
//!
//! Its requests carry the bearer token, the dashboard PIN and the read proof
//! (`X-Mentis-Auth`, a signature bound to method, path and query). ureq forwards
//! custom headers to wherever a redirect points, so a daemon (or whatever
//! answers at `--url`) that redirects would hand the read proof to another
//! host. These tests point the CLI at a server that answers `302` to a second
//! server and check that the second one is never contacted.

use axum::http::{HeaderMap, StatusCode};
use axum::response::Redirect;
use axum::routing::any;
use std::io::Cursor;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

/// A listener that records the header names of every request it receives.
async fn recorder() -> (String, Arc<Mutex<Vec<Vec<String>>>>) {
    let seen: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let sink = seen.clone();
    let router = axum::Router::new().fallback(any(move |headers: HeaderMap| {
        let sink = sink.clone();
        async move {
            sink.lock()
                .unwrap()
                .push(headers.keys().map(|k| k.as_str().to_string()).collect());
            (StatusCode::OK, "{}")
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, seen)
}

/// A listener that answers every request with `status` redirecting to `target`.
async fn redirector(target: String, status: u16) -> String {
    let router = axum::Router::new().fallback(any(move || {
        let target = target.clone();
        async move {
            match status {
                301 => Redirect::permanent(&target),
                303 => Redirect::to(&target),
                _ => Redirect::temporary(&target),
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

async fn cli(args: Vec<String>) -> (bool, String, String) {
    tokio::task::spawn_blocking(move || {
        let argv = std::iter::once("mentisdb".to_string()).chain(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = mentisdb::cli::run_with_io(argv, &mut Cursor::new(""), &mut out, &mut err);
        (
            format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    })
    .await
    .unwrap()
}

fn seed_file() -> String {
    let path = std::env::temp_dir().join(format!(
        "mentisdb_redirect_seed_{}_{}.key",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, format!("{}\n", "07".repeat(32))).unwrap();
    path.to_str().unwrap().to_string()
}

/// Regression: a `301`, `302`, `303` or `307` from the daemon address did not
/// stop the CLI; it re-sent the request, `X-Mentis-Auth` included, to the
/// redirect target. Now the target is never contacted and the command fails
/// with a message naming the redirect.
#[tokio::test(flavor = "multi_thread")]
async fn a_redirect_never_receives_the_read_proof_or_the_token() {
    let seed = seed_file();
    for status in [301u16, 302, 303] {
        let (target, seen) = recorder().await;
        let daemon = redirector(format!("{target}/stolen"), status).await;
        let (ok, _out, err) = cli(vec![
            "chain".into(),
            "access".into(),
            "verify".into(),
            "--chain".into(),
            "vault".into(),
            "--read-key-file".into(),
            seed.clone(),
            "--read-agent".into(),
            "agent".into(),
            "--read-key-id".into(),
            "k1".into(),
            "--token".into(),
            "secret-bearer".into(),
            "--url".into(),
            daemon,
            "--json".into(),
        ])
        .await;
        assert!(!ok, "a redirect is an error ({status})");
        assert!(err.contains("redirect"), "{status}: {err}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "the redirect target was contacted after a {status}: {:?}",
            seen.lock().unwrap()
        );
    }
    let _ = std::fs::remove_file(seed);
}

/// Sanity: the same command against a server that does not redirect does send
/// the read proof (so the test above is not passing for lack of a header).
#[tokio::test(flavor = "multi_thread")]
async fn without_a_redirect_the_read_proof_is_sent() {
    let seed = seed_file();
    let (daemon, seen) = recorder().await;
    let _ = cli(vec![
        "chain".into(),
        "access".into(),
        "verify".into(),
        "--chain".into(),
        "vault".into(),
        "--read-key-file".into(),
        seed.clone(),
        "--read-agent".into(),
        "agent".into(),
        "--read-key-id".into(),
        "k1".into(),
        "--url".into(),
        daemon,
        "--json".into(),
    ])
    .await;
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|names| names.iter().any(|n| n == "x-mentis-auth")),
        "the proof header is sent to the named daemon: {seen:?}"
    );
    let _ = std::fs::remove_file(seed);
}
