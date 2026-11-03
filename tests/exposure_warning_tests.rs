#![cfg(feature = "server")]
//! Startup warnings for a daemon that other machines can reach without full
//! authentication (the Docker image binds `0.0.0.0` with bearer tokens and the
//! dashboard PIN off). The daemon still starts: the warning names the fix.

use mentisdb::server::{start_servers, MentisDbServerConfig};
use mentisdb::StorageAdapterKind;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;

/// `start_servers` stores process-wide exposure flags the dashboard reads, so
/// the tests that start a daemon take turns.
static START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn addr(text: &str) -> SocketAddr {
    text.parse().unwrap()
}

/// A config with every listener on loopback, bearer off, no PIN.
fn loopback_config(dir: &std::path::Path) -> MentisDbServerConfig {
    let mut config = MentisDbServerConfig::from_env();
    config.service = mentisdb::server::MentisDbServiceConfig::new(
        dir.to_path_buf(),
        "default",
        StorageAdapterKind::Binary,
    );
    config
        .service
        .bearer_token_access
        .store(false, Ordering::SeqCst);
    config.mcp_addr = addr("127.0.0.1:0");
    config.rest_addr = addr("127.0.0.1:0");
    config.https_mcp_addr = None;
    config.https_rest_addr = None;
    config.dashboard_addr = None;
    config.dashboard_pin = None;
    config.tls_cert_path = dir.join("tls/cert.pem");
    config.tls_key_path = dir.join("tls/key.pem");
    config
}

#[test]
fn a_loopback_daemon_has_no_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.dashboard_addr = Some(addr("127.0.0.1:0"));
    config.https_mcp_addr = Some(addr("[::1]:0"));
    assert!(!config.network_exposed());
    assert!(config.exposure_warnings().is_empty());
}

#[test]
fn a_wildcard_bind_without_bearer_tokens_warns_and_names_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.mcp_addr = addr("0.0.0.0:9471");
    config.rest_addr = addr("0.0.0.0:9472");
    assert!(config.network_exposed());
    let warnings = config.exposure_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("MENTISDB_BEARER_TOKEN_ACCESS=true"));
    assert!(
        warnings[0].contains("-p 127.0.0.1:9471:9471"),
        "Docker advice"
    );
}

#[test]
fn bearer_enforcement_silences_the_api_warning_but_not_the_pin_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.mcp_addr = addr("192.168.1.20:9471");
    config.dashboard_addr = Some(addr("0.0.0.0:9475"));
    config
        .service
        .bearer_token_access
        .store(true, Ordering::SeqCst);
    let warnings = config.exposure_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("MENTISDB_DASHBOARD_PIN"));
    config.dashboard_pin = Some("a-long-pin".into());
    assert!(config.exposure_warnings().is_empty());
}

#[test]
fn each_unauthenticated_exposed_surface_gets_its_own_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.https_rest_addr = Some(addr("[::]:9474"));
    config.dashboard_addr = Some(addr("0.0.0.0:9475"));
    let warnings = config.exposure_warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("BEARER_TOKEN_ACCESS")));
    assert!(warnings.iter().any(|w| w.contains("DASHBOARD_PIN")));
}

/// The dashboard shows the same warning as a banner: `GET
/// /dashboard/api/version` carries `warnings`, empty on a loopback daemon.
#[tokio::test]
async fn the_dashboard_version_endpoint_carries_the_warnings() {
    let _lock = START_LOCK.lock().await;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    // Loopback: no warnings.
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.dashboard_addr = Some(addr("127.0.0.1:0"));
    let mut handles = start_servers(config, None).await.unwrap();
    let port = handles.dashboard.as_ref().unwrap().local_addr().port();
    let body: serde_json::Value = client
        .get(format!("https://127.0.0.1:{port}/dashboard/api/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["warnings"], serde_json::json!([]), "{body}");
    assert!(body["version"].is_string());
    let _ = handles.mcp.shutdown();
    let _ = handles.rest.shutdown();
    let _ = handles.dashboard.as_mut().unwrap().shutdown();

    // Dashboard on a wildcard bind with no PIN: it warns (reached over loopback here).
    let dir = tempfile::tempdir().unwrap();
    let mut config = loopback_config(dir.path());
    config.dashboard_addr = Some(addr("0.0.0.0:0"));
    let mut handles = start_servers(config, None).await.unwrap();
    let port = handles.dashboard.as_ref().unwrap().local_addr().port();
    let body: serde_json::Value = client
        .get(format!("https://127.0.0.1:{port}/dashboard/api/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let warnings = body["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{body}");
    assert!(warnings[0].as_str().unwrap().contains("DASHBOARD_PIN"));
    let _ = handles.mcp.shutdown();
    let _ = handles.rest.shutdown();
    let _ = handles.dashboard.as_mut().unwrap().shutdown();
}
