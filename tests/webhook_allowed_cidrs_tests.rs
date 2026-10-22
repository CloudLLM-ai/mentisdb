//! Integration tests for `MENTISDB_WEBHOOK_ALLOWED_CIDRS` (GitHub issue #22):
//! operator-allowlisted private ranges for self-hosted webhook consumers.
#![cfg(feature = "server")]

use chrono::Utc;
use mentisdb::webhooks::{
    spawn_delivery_worker, validate_webhook_url_with, DeliveryJob, WebhookCidr, WebhookManager,
    WebhookPayload, WebhookThought, WebhookUrlPolicy,
};
use mentisdb::ThoughtType;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use uuid::Uuid;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn cidr(text: &str) -> WebhookCidr {
    text.parse().unwrap()
}

/// CIDR entries parse for IPv4, IPv6, bracketed IPv6 and bare host
/// addresses, and host bits are cleared so the canonical form is the network.
#[test]
fn test_webhook_cidr_parses_and_normalizes() {
    assert_eq!(cidr("100.64.0.0/10").to_string(), "100.64.0.0/10");
    assert_eq!(cidr("100.100.7.9/10").to_string(), "100.64.0.0/10");
    assert_eq!(cidr("10.8.0.5").to_string(), "10.8.0.5/32");
    assert_eq!(
        cidr("fd7a:115c:a1e0::1/48").to_string(),
        "fd7a:115c:a1e0::/48"
    );
    assert_eq!(cidr("[fd00::1]").to_string(), "fd00::1/128");
    assert_eq!(cidr("100.64.0.0/10").prefix_len(), 10);
    assert_eq!(cidr("100.64.0.0/10").network(), ip("100.64.0.0"));
}

/// Malformed entries, out-of-range prefixes and ranges broader than /8 (which
/// would re-open most of the address space the SSRF guard protects) are
/// rejected with an error naming the entry.
#[test]
fn test_webhook_cidr_rejects_invalid_entries() {
    for bad in [
        "not-an-ip",
        "10.0.0.0/33",
        "fd00::/129",
        "10.0.0.0/x",
        "10.0.0.0/",
        "0.0.0.0/0",
        "::/0",
        "0.0.0.0/1",
        "128.0.0.0/1",
        "10.0.0.0/7",
        "::/1",
        "fc00::/7",
    ] {
        let error = bad.parse::<WebhookCidr>().expect_err(bad);
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{bad}");
        assert!(error.to_string().contains(bad), "{bad}: {error}");
        assert!(
            error.to_string().contains("MENTISDB_WEBHOOK_ALLOWED_CIDRS"),
            "{error}"
        );
    }
}

/// Membership honours the prefix boundary and matches IPv4-mapped IPv6
/// addresses against IPv4 ranges, never mixing address families otherwise.
#[test]
fn test_webhook_cidr_contains_respects_prefix_and_family() {
    let tailnet = cidr("100.64.0.0/10");
    assert!(tailnet.contains(&ip("100.64.0.1")));
    assert!(tailnet.contains(&ip("100.127.255.254")));
    assert!(!tailnet.contains(&ip("100.128.0.1")));
    assert!(!tailnet.contains(&ip("100.63.255.255")));
    assert!(tailnet.contains(&ip("::ffff:100.101.1.2")));
    assert!(!tailnet.contains(&ip("fd7a:115c:a1e0::1")));

    let ula = cidr("fd7a:115c:a1e0::/48");
    assert!(ula.contains(&ip("fd7a:115c:a1e0:ab12::1")));
    assert!(!ula.contains(&ip("fd7a:115c:a1e1::1")));
    assert!(!ula.contains(&ip("100.64.0.1")));
}

/// The broadest accepted ranges are exactly /8 for both address families.
#[test]
fn test_webhook_cidr_accepts_prefixes_at_the_minimum() {
    assert_eq!(cidr("10.0.0.0/8").prefix_len(), 8);
    assert_eq!(cidr("fd00::/8").prefix_len(), 8);
    assert_eq!(mentisdb::webhooks::MIN_ALLOWED_PREFIX_V4, 8);
    assert_eq!(mentisdb::webhooks::MIN_ALLOWED_PREFIX_V6, 8);
}

/// The list accepts commas and/or whitespace; blank means the strict default;
/// one bad entry fails the whole list rather than silently dropping it.
#[test]
fn test_policy_from_allowed_cidrs_parses_lists() {
    let policy =
        WebhookUrlPolicy::from_allowed_cidrs(" 100.64.0.0/10,10.8.0.0/24  fd00::/8 ,").unwrap();
    match policy {
        WebhookUrlPolicy::AllowCidrs(cidrs) => {
            let rendered: Vec<String> = cidrs.iter().map(ToString::to_string).collect();
            assert_eq!(rendered, ["100.64.0.0/10", "10.8.0.0/24", "fd00::/8"]);
        }
        other => panic!("expected AllowCidrs, got {other:?}"),
    }
    assert_eq!(
        WebhookUrlPolicy::from_allowed_cidrs("").unwrap(),
        WebhookUrlPolicy::Strict
    );
    assert_eq!(
        WebhookUrlPolicy::from_allowed_cidrs(" , ").unwrap(),
        WebhookUrlPolicy::Strict
    );
    assert!(WebhookUrlPolicy::from_allowed_cidrs("100.64.0.0/10,bogus").is_err());
}

/// Allowlisted ranges open only themselves: every other internal range stays
/// blocked and public addresses stay allowed.
#[test]
fn test_allow_cidrs_policy_opens_only_listed_ranges() {
    let policy = WebhookUrlPolicy::from_allowed_cidrs("100.64.0.0/10,10.8.0.0/24").unwrap();
    assert!(policy.permits_ip(&ip("100.101.102.103")));
    assert!(policy.permits_ip(&ip("10.8.0.42")));
    assert!(policy.permits_ip(&ip("8.8.8.8")));
    for blocked in [
        "10.9.0.1",
        "192.168.1.1",
        "127.0.0.1",
        "169.254.169.254",
        "::1",
        "fd00::1",
    ] {
        assert!(!policy.permits_ip(&ip(blocked)), "{blocked}");
    }
    assert!(!WebhookUrlPolicy::Strict.permits_ip(&ip("100.101.102.103")));
}

/// URL validation applies the allowlist to literal IPs but still rejects
/// `localhost` host names even when loopback is allowlisted.
#[test]
fn test_validate_webhook_url_with_allow_cidrs() {
    let policy = WebhookUrlPolicy::from_allowed_cidrs("100.64.0.0/10,127.0.0.0/8").unwrap();
    assert!(validate_webhook_url_with("http://100.101.1.2:8080/hook", policy.clone()).is_ok());
    assert!(validate_webhook_url_with("http://127.0.0.1:9000/hook", policy.clone()).is_ok());
    assert!(validate_webhook_url_with("http://[::ffff:100.101.1.2]/hook", policy.clone()).is_ok());
    assert!(validate_webhook_url_with("https://example.com/hook", policy.clone()).is_ok());
    assert!(validate_webhook_url_with("http://localhost:9000/hook", policy.clone()).is_err());
    assert!(validate_webhook_url_with("http://10.0.0.1/hook", policy.clone()).is_err());
    assert!(validate_webhook_url_with("ftp://100.101.1.2/hook", policy).is_err());
    assert!(
        validate_webhook_url_with("http://100.101.1.2/hook", WebhookUrlPolicy::Strict).is_err()
    );
}

/// A manager under the allowlist registers overlay targets, and persisted
/// registrations stay active under it but load inactive under the strict
/// default (so removing the env var re-closes the range).
#[test]
fn test_manager_registers_and_reloads_allowlisted_targets() {
    let dir = tempdir().unwrap();
    let policy = WebhookUrlPolicy::from_allowed_cidrs("100.64.0.0/10").unwrap();
    let manager =
        WebhookManager::new_with_policy(dir.path().to_path_buf(), policy.clone()).unwrap();
    let registration = manager
        .register_webhook("http://100.101.1.2:8080/hook".to_string(), None, None)
        .unwrap();
    assert!(manager
        .register_webhook("http://10.0.0.1/hook".to_string(), None, None)
        .is_err());

    let reloaded = WebhookManager::new_with_policy(dir.path().to_path_buf(), policy).unwrap();
    assert!(reloaded.get_webhook(registration.id).unwrap().active);

    let strict = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    assert!(!strict.get_webhook(registration.id).unwrap().active);
}

fn sample_payload() -> WebhookPayload {
    WebhookPayload {
        event: "thought.appended".to_string(),
        chain_key: "test-chain".to_string(),
        thought: WebhookThought {
            id: Uuid::new_v4(),
            thought_type: ThoughtType::Finding,
            content: "payload".to_string(),
            importance: 0.5,
            confidence: None,
            tags: Vec::new(),
            concepts: Vec::new(),
            agent_id: "agent".to_string(),
            index: 1,
            timestamp: Utc::now(),
        },
        timestamp: Utc::now(),
    }
}

async fn spawn_counting_server() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer)
                        .await
                        .unwrap_or(0);
                    if read == 0 || buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut socket,
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            });
        }
    });
    (address, hits)
}

/// End to end: the delivery worker reaches a target inside an allowlisted
/// range, and the strict worker still refuses the same target.
#[tokio::test]
async fn test_delivery_reaches_allowlisted_target_only() {
    let (address, hits) = spawn_counting_server().await;
    let allow = WebhookUrlPolicy::from_allowed_cidrs("127.0.0.1/32").unwrap();
    let allowed_sender = spawn_delivery_worker(allow);
    allowed_sender
        .send(DeliveryJob {
            url: format!("http://{address}/hook"),
            payload: sample_payload(),
            webhook_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while hits.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let strict_sender = spawn_delivery_worker(WebhookUrlPolicy::Strict);
    strict_sender
        .send(DeliveryJob {
            url: format!("http://{address}/hook"),
            payload: sample_payload(),
            webhook_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
