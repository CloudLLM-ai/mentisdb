#![cfg(feature = "server")]
//! The dashboard PIN limiter: IPv6 clients are limited per `/64`, the table of
//! remembered clients is bounded, and a global failure budget with backoff caps
//! an attacker who rotates through many addresses.
//!
//! The limiter's clock is a parameter, so the time-dependent tests are
//! deterministic (no sleeping).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

use dashboard_impl::{
    limiter_key, PinLimiter, GLOBAL_BACKOFF_BASE_SECS, GLOBAL_BACKOFF_MAX_SECS,
    GLOBAL_FAILURE_BUDGET, RATE_LIMIT_MAX_ATTEMPTS, RATE_LIMIT_MAX_TRACKED, RATE_LIMIT_WINDOW_SECS,
};

fn key(ip: &str) -> String {
    limiter_key(ip.parse().unwrap())
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

// ── per client ──────────────────────────────────────────────────────────────

/// IPv4 clients are keyed by address, IPv4-mapped IPv6 as the IPv4 address, and
/// IPv6 clients by `/64`.
#[test]
fn limiter_keys_group_ipv6_by_slash_64() {
    assert_eq!(key("203.0.113.7"), "203.0.113.7");
    assert_eq!(key("::ffff:203.0.113.7"), "203.0.113.7");
    // Rotating the interface identifier stays one client.
    assert_eq!(
        key("2001:db8:0:1::1"),
        key("2001:db8:0:1:aaaa:bbbb:cccc:dddd")
    );
    // Another /64 is another client, and so is another /48.
    assert_ne!(key("2001:db8:0:1::1"), key("2001:db8:0:2::1"));
    assert_ne!(key("2001:db8:1:1::1"), key("2001:db8:2:1::1"));
}

/// Regression: the limiter keyed IPv6 clients by full address, so an attacker
/// with a `/64` (a single residential customer) rotated source addresses and
/// never reached the limit.
#[test]
fn rotating_addresses_inside_one_slash_64_does_not_reset_the_limit() {
    let mut limiter = PinLimiter::new();
    let now = Instant::now();
    for guess in 0..RATE_LIMIT_MAX_ATTEMPTS {
        let rotated = key(&format!("2001:db8:0:9::{:x}", guess + 1));
        assert_eq!(limiter.blocked(&rotated, now), None, "attempt {guess}");
        limiter.record_failure(&rotated, now);
    }
    // A brand-new address in the same /64 is blocked; another /64 is not.
    assert!(limiter.blocked(&key("2001:db8:0:9::ffff"), now).is_some());
    assert_eq!(limiter.blocked(&key("2001:db8:0:a::1"), now), None);
    assert_eq!(limiter.blocked(&key("203.0.113.9"), now), None);
}

/// The block is for a window, counted from the latest failure; a correct PIN
/// clears the client.
#[test]
fn a_client_is_unblocked_after_the_window_and_by_a_correct_pin() {
    let mut limiter = PinLimiter::new();
    let now = Instant::now();
    let client = key("198.51.100.4");
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        limiter.record_failure(&client, now);
    }
    let wait = limiter.blocked(&client, now).expect("blocked");
    assert!((1..=RATE_LIMIT_WINDOW_SECS).contains(&wait), "{wait}");
    assert!(limiter
        .blocked(&client, now + secs(RATE_LIMIT_WINDOW_SECS - 1))
        .is_some());
    assert_eq!(
        limiter.blocked(&client, now + secs(RATE_LIMIT_WINDOW_SECS)),
        None
    );
    assert_eq!(limiter.tracked(), 0, "the expired entry was dropped");

    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS - 1 {
        limiter.record_failure(&client, now);
    }
    limiter.record_success(&client);
    assert_eq!(limiter.tracked(), 0);
}

// ── bounded memory ──────────────────────────────────────────────────────────

/// Regression: only expired entries were evicted, so a flood of distinct
/// addresses inside one window grew the table without bound.
#[test]
fn the_table_of_remembered_clients_is_bounded_and_evicts_the_oldest() {
    let mut limiter = PinLimiter::new();
    let start = Instant::now();
    // One failure per client, a millisecond apart, all inside one window, so
    // nothing has expired and "oldest" is well defined. (Whether the global
    // budget trips on the way is not what is under test here.)
    let total = RATE_LIMIT_MAX_TRACKED + 500;
    for n in 0..total {
        limiter.record_failure(
            &format!("client-{n}"),
            start + Duration::from_millis(n as u64),
        );
        assert!(limiter.tracked() <= RATE_LIMIT_MAX_TRACKED, "after {n}");
    }
    assert_eq!(limiter.tracked(), RATE_LIMIT_MAX_TRACKED);
    assert!(!limiter.remembers("client-0"), "the oldest was evicted");
    assert!(!limiter.remembers("client-499"), "so were the next oldest");
    assert!(limiter.remembers("client-500"));
    assert!(
        limiter.remembers(&format!("client-{}", total - 1)),
        "the newest stays"
    );
}

// ── global budget and backoff ───────────────────────────────────────────────

fn spend_budget(limiter: &mut PinLimiter, now: Instant, prefix: &str) {
    for n in 0..=GLOBAL_FAILURE_BUDGET {
        limiter.record_failure(&format!("{prefix}-{n}"), now);
    }
}

/// An attacker rotating through many addresses stays under every per-client
/// limit, but the global budget stops them: every PIN attempt, from any
/// address, waits.
#[test]
fn the_global_budget_locks_every_attempt_even_across_many_addresses() {
    let mut limiter = PinLimiter::new();
    let now = Instant::now();
    // One failure per address: no per-client limit is ever reached.
    for n in 0..GLOBAL_FAILURE_BUDGET {
        limiter.record_failure(&format!("10.0.{}.{}", n / 250, n % 250), now);
        assert_eq!(limiter.blocked("203.0.113.200", now), None, "failure {n}");
    }
    limiter.record_failure("10.9.9.9", now);
    for client in ["203.0.113.200", "10.9.9.9", "someone-new"] {
        let wait = limiter.blocked(client, now).expect("globally locked");
        assert!(wait <= GLOBAL_BACKOFF_BASE_SECS, "{wait}");
    }
    // It lifts after the backoff.
    assert_eq!(
        limiter.blocked("203.0.113.200", now + secs(GLOBAL_BACKOFF_BASE_SECS)),
        None
    );
}

/// Failures spread over time never trip the budget: the count restarts with
/// every window.
#[test]
fn a_slow_trickle_of_failures_does_not_trip_the_global_budget() {
    let mut limiter = PinLimiter::new();
    let start = Instant::now();
    for n in 0..200u64 {
        let now = start + secs(n * (RATE_LIMIT_WINDOW_SECS / 4));
        limiter.record_failure(&format!("client-{n}"), now);
        assert_eq!(limiter.blocked("owner", now), None, "failure {n}");
    }
}

/// Each repeat doubles the lockout, up to the cap; a quiet spell forgives.
#[test]
fn the_backoff_doubles_to_a_cap_and_is_forgiven_after_quiet() {
    let mut limiter = PinLimiter::new();
    let mut now = Instant::now();
    let mut expected = GLOBAL_BACKOFF_BASE_SECS;
    for strike in 0..12 {
        spend_budget(&mut limiter, now, &format!("s{strike}"));
        let wait = limiter.blocked("owner", now).expect("locked");
        assert_eq!(
            wait,
            expected.min(GLOBAL_BACKOFF_MAX_SECS),
            "strike {strike}"
        );
        now += secs(wait);
        assert_eq!(limiter.blocked("owner", now), None, "lifted after {wait}s");
        expected = (expected * 2).min(GLOBAL_BACKOFF_MAX_SECS);
    }
    // A long quiet spell resets the escalation.
    now += secs(GLOBAL_BACKOFF_MAX_SECS * 2 + 1);
    spend_budget(&mut limiter, now, "after-quiet");
    assert_eq!(
        limiter.blocked("owner", now),
        Some(GLOBAL_BACKOFF_BASE_SECS)
    );
}

/// The global lockout refuses a correct PIN too (the limiter runs before the
/// comparison), and a success does not refund the budget.
#[test]
fn a_success_does_not_refund_the_global_budget() {
    let mut limiter = PinLimiter::new();
    let now = Instant::now();
    for n in 0..GLOBAL_FAILURE_BUDGET {
        limiter.record_failure(&format!("a-{n}"), now);
        limiter.record_success("owner");
    }
    limiter.record_failure("a-last", now);
    assert!(limiter.blocked("owner", now).is_some());
}

// ── over HTTP ───────────────────────────────────────────────────────────────

fn router(pin: &str) -> (axum::Router, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_pin_limiter_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let app = dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::<String, Arc<RwLock<MentisDb>>>::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: Some(pin.to_string()),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    });
    (app, dir)
}

async fn login(app: &axum::Router, peer: std::net::SocketAddr, pin: &str) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri("/dashboard/login")
        .header("host", "localhost:9475")
        .header("x-mentisdb-dashboard", "1")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(format!("pin={pin}")))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));
    req.extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
    app.clone().oneshot(req).await.unwrap().status()
}

fn v6(last: u16) -> std::net::SocketAddr {
    // A /64 of its own for this test (2001:db8:aaaa:1::/64).
    std::net::SocketAddr::from(([0x2001, 0xdb8, 0xaaaa, 1, 0, 0, 0, last], 40000))
}

/// Through the real login handler: five wrong PINs from rotating addresses of
/// one `/64` lock out the whole `/64`, and the answer says how long to wait.
#[tokio::test]
async fn the_login_form_limits_an_ipv6_slash_64_as_one_client() {
    let (app, dir) = router("right-pin");
    for last in 1..=RATE_LIMIT_MAX_ATTEMPTS as u16 {
        assert_eq!(
            login(&app, v6(last), "wrong").await,
            StatusCode::SEE_OTHER,
            "wrong PIN {last} redirects back to the login page"
        );
    }
    // Even the right PIN from a never-seen address of that /64 is refused.
    assert_eq!(
        login(&app, v6(0xbeef), "right-pin").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // The owner on IPv4 is untouched.
    let owner = std::net::SocketAddr::from(([198, 51, 100, 77], 40000));
    assert_eq!(login(&app, owner, "right-pin").await, StatusCode::SEE_OTHER);
    let _ = std::fs::remove_dir_all(dir);
}

/// The `429` carries `Retry-After`.
#[tokio::test]
async fn the_refusal_carries_retry_after() {
    let (app, dir) = router("right-pin");
    let peer = std::net::SocketAddr::from(([198, 51, 100, 200], 40000));
    for _ in 0..RATE_LIMIT_MAX_ATTEMPTS {
        login(&app, peer, "wrong").await;
    }
    let mut req = Request::builder()
        .method("POST")
        .uri("/dashboard/login")
        .header("host", "localhost:9475")
        .header("x-mentisdb-dashboard", "1")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("pin=wrong"))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));
    req.extensions_mut()
        .insert(dashboard_impl::ListenerTls(true));
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response
        .headers()
        .get("retry-after")
        .expect("Retry-After")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=RATE_LIMIT_WINDOW_SECS).contains(&wait), "{wait}");
    let _ = std::fs::remove_dir_all(dir);
}
