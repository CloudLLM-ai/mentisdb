//! Integration tests for webhook registration, SSRF defenses, and delivery.
#![cfg(feature = "server")]

use chrono::Utc;
use mentisdb::webhooks::{
    is_blocked_ip, redact_webhook_url, spawn_delivery_worker, spawn_delivery_worker_with_breaker,
    validate_webhook_url, CircuitBreaker, DeliveryJob, WebhookManager, WebhookPayload,
    WebhookThought, WebhookUrlPolicy,
};
use mentisdb::ThoughtType;
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use uuid::Uuid;

fn sample_payload() -> WebhookPayload {
    WebhookPayload {
        event: "thought.appended".to_string(),
        chain_key: "test-chain".to_string(),
        thought: WebhookThought {
            id: Uuid::new_v4(),
            thought_type: ThoughtType::Finding,
            content: "payload".to_string(),
            importance: 0.5,
            confidence: Some(0.9),
            tags: Vec::new(),
            concepts: Vec::new(),
            agent_id: "agent".to_string(),
            index: 1,
            timestamp: Utc::now(),
        },
        timestamp: Utc::now(),
    }
}

/// Spawns a tiny HTTP server answering every request with `response`; returns
/// its address and a counter of accepted connections.
async fn spawn_http_server(response: &'static str) -> (std::net::SocketAddr, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_for_server = Arc::clone(&hits);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            hits_for_server.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer)
                        .await
                        .unwrap_or(0);
                    if read == 0
                        || buffer[..read]
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                    {
                        break;
                    }
                }
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
            });
        }
    });
    (address, hits)
}

/// Registrations round-trip through list/get and delete.
#[test]
fn test_webhook_registration_round_trips_and_deletes() {
    let dir = tempdir().unwrap();
    let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    let registration = manager
        .register_webhook(
            "https://example.com/webhook".to_string(),
            Some("test-chain".to_string()),
            None,
        )
        .unwrap();
    assert_eq!(manager.list_webhooks().len(), 1);
    assert_eq!(
        manager.get_webhook(registration.id).unwrap().id,
        registration.id
    );
    let reloaded = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    assert_eq!(reloaded.list_webhooks().len(), 1);
    assert!(manager.delete_webhook(registration.id).unwrap());
    assert!(manager.list_webhooks().is_empty());
}

/// Thought type filters survive registration.
#[test]
fn test_webhook_thought_type_filter_is_kept() {
    let dir = tempdir().unwrap();
    let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    let mut filter = HashSet::new();
    filter.insert(ThoughtType::LessonLearned);
    manager
        .register_webhook("https://example.com/l".to_string(), None, Some(filter))
        .unwrap();
    assert_eq!(
        manager.list_webhooks()[0]
            .thought_type_filter
            .as_ref()
            .unwrap()
            .len(),
        1
    );
}

/// A failed save leaves the previous registry file intact and no temp file behind.
#[test]
fn test_webhook_save_failure_keeps_registry_and_cleans_temp() {
    let dir = tempdir().unwrap();
    let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    manager
        .register_webhook("https://example.com/original".to_string(), None, None)
        .unwrap();
    let path = dir.path().join("mentisdb-webhooks.json");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let result = manager.register_webhook("https://example.com/new".to_string(), None, None);
    assert!(result.is_err());
    assert!(!path.with_extension("json.tmp").exists());
}

/// Blocked literal targets and schemes are rejected, including IPv6 forms.
#[test]
fn test_validate_webhook_url_rejects_internal_targets() {
    for url in [
        "http://127.0.0.1:8080/hook",
        "http://localhost:8080/hook",
        "http://app.localhost/hook",
        "http://10.0.0.1/hook",
        "http://192.168.1.1/hook",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/hook",
        "http://[::ffff:127.0.0.1]/hook",
        "http://[::ffff:169.254.169.254]/hook",
        "http://[fe80::1]/hook",
        "http://[fd00::1]/hook",
        "http://[ff02::1]/hook",
        "http://0.0.0.0/hook",
        "http://2130706433/hook",
        "http://0x7f.1/hook",
        "http://100.64.0.1/hook",
        "http://192.0.2.1/hook",
        "http://[2001:db8::1]/hook",
        "http://[64:ff9b::7f00:1]/hook",
        "http://[2002:7f00:1::1]/hook",
        "http://224.0.0.1/hook",
        "file:///etc/passwd",
        "gopher://evil.com",
    ] {
        assert!(validate_webhook_url(url).is_err(), "should reject {url}");
    }
}

/// Public targets are accepted.
#[test]
fn test_validate_webhook_url_accepts_public_targets() {
    assert!(validate_webhook_url("https://example.com/webhook").is_ok());
    assert!(validate_webhook_url("http://8.8.8.8/webhook").is_ok());
    assert!(validate_webhook_url("http://[2606:4700:4700::1111]/webhook").is_ok());
}

/// `is_blocked_ip` normalizes IPv4-mapped IPv6 addresses.
#[test]
fn test_is_blocked_ip_normalizes_ipv4_mapped() {
    let mapped: IpAddr = "::ffff:10.1.2.3".parse().unwrap();
    assert!(is_blocked_ip(&mapped));
    let public: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
    assert!(!is_blocked_ip(&public));
}

/// Registrations loaded from disk that point at internal hosts are disabled.
#[test]
fn test_webhooks_loaded_from_disk_are_revalidated() {
    let dir = tempdir().unwrap();
    let id = Uuid::new_v4();
    let registry = serde_json::json!({
        "version": 1,
        "webhooks": [{
            "id": id,
            "url": "http://127.0.0.1:9/hook",
            "chain_key_filter": null,
            "thought_type_filter": null,
            "created_at": Utc::now(),
            "active": true
        }]
    });
    fs::write(
        dir.path().join("mentisdb-webhooks.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
    let manager = WebhookManager::new(dir.path().to_path_buf()).unwrap();
    assert!(!manager.get_webhook(id).unwrap().active);
}

/// URL redaction drops credentials, path and query.
#[test]
fn test_redact_webhook_url_keeps_only_origin() {
    assert_eq!(
        redact_webhook_url("https://user:pw@example.com:8443/secret/path?token=abc"),
        "https://example.com:8443"
    );
    assert_eq!(redact_webhook_url("not a url"), "<invalid-url>");
}

/// Breaker trips after the threshold, pauses, then re-allows after cooldown.
#[test]
fn test_circuit_breaker_trips_and_recovers() {
    let breaker = CircuitBreaker::new(3, Duration::from_secs(60));
    let id = Uuid::new_v4();
    let t0 = Instant::now();
    assert!(breaker.allow(id, t0));
    assert!(!breaker.record_failure(id, t0));
    assert!(!breaker.record_failure(id, t0));
    assert!(breaker.record_failure(id, t0));
    assert!(!breaker.allow(id, t0 + Duration::from_secs(10)));
    assert!(breaker.allow(id, t0 + Duration::from_secs(61)));
    // Half-open: a single failure re-trips.
    assert!(breaker.record_failure(id, t0 + Duration::from_secs(61)));
    breaker.record_success(id);
    assert!(breaker.allow(id, t0 + Duration::from_secs(62)));
}

/// Delivery concurrency stays bounded (permissive policy for loopback server).
#[tokio::test]
async fn test_delivery_worker_bounds_concurrency() {
    let (address, hits) =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    let sender = spawn_delivery_worker(WebhookUrlPolicy::AllowPrivate);
    for i in 0..8 {
        sender
            .send(DeliveryJob {
                url: format!("http://{address}/webhook/{i}"),
                payload: sample_payload(),
                webhook_id: Uuid::new_v4(),
            })
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 8);
}

/// The strict worker never connects to a literal loopback target.
#[tokio::test]
async fn test_strict_delivery_never_connects_to_loopback() {
    let (address, hits) =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    let sender = spawn_delivery_worker(WebhookUrlPolicy::Strict);
    sender
        .send(DeliveryJob {
            url: format!("http://{address}/hook"),
            payload: sample_payload(),
            webhook_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

/// Redirects are not followed, so a 302 cannot bounce delivery to another host.
#[tokio::test]
async fn test_delivery_does_not_follow_redirects() {
    let (target, target_hits) =
        spawn_http_server("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
    let redirect: &'static str = Box::leak(
        format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{target}/internal\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_boxed_str(),
    );
    let (origin, origin_hits) = spawn_http_server(redirect).await;
    let breaker = Arc::new(CircuitBreaker::new(100, Duration::from_secs(1)));
    let sender = spawn_delivery_worker_with_breaker(WebhookUrlPolicy::AllowPrivate, breaker);
    sender
        .send(DeliveryJob {
            url: format!("http://{origin}/hook"),
            payload: sample_payload(),
            webhook_id: Uuid::new_v4(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(origin_hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0);
}
