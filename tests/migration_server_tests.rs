//! End to end: a daemon with a background migration manager serves every other
//! chain while one migrates, answers the migrating chain with 503 and
//! `Retry-After`, reports progress on `GET /v1/migrations` and the
//! `mentisdb_migration_status` tool, and serves the chain once it is done.

#![cfg(feature = "server")]

use mentisdb::migration::fixtures::{semantic_chain, write_fixture, FixtureFormat};
use mentisdb::migration::{Failpoint, MigrationConfig, MigrationManager, MigrationOptions};
use mentisdb::server::{
    start_servers_with_migrations, MentisDbServerConfig, MentisDbServiceConfig,
};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

fn put(dir: &Path, key: &str, format: FixtureFormat, count: usize) {
    write_fixture(
        &dir.join(chain_storage_filename(key, StorageAdapterKind::Binary)),
        format,
        &semantic_chain(count),
    )
    .unwrap();
}

/// A reply: status, `Retry-After`, parsed JSON body.
struct Reply {
    status: u16,
    retry_after: Option<String>,
    body: Value,
}

fn call(method: &str, url: String, body: Option<Value>) -> Reply {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = match (method, body) {
        ("GET", _) => agent.get(&url).call(),
        (_, body) => agent
            .post(&url)
            .header("content-type", "application/json")
            .send(body.unwrap_or_else(|| json!({})).to_string()),
    }
    .expect("request");
    Reply {
        status: response.status().as_u16(),
        retry_after: response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        body: response.body_mut().read_json().unwrap_or(Value::Null),
    }
}

async fn rest(method: &'static str, url: String, body: Option<Value>) -> Reply {
    tokio::task::spawn_blocking(move || call(method, url, body))
        .await
        .unwrap()
}

/// A latch that holds migrations open until the test releases it, so every
/// assertion about "a chain is migrating" is deterministic (no timing).
#[derive(Default)]
struct Gate {
    open: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Gate {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

/// Start a daemon over `dir`. With a `gate`, every migration blocks at the
/// first record of its chain until the gate is released.
async fn daemon(dir: &Path, gate: Option<Arc<Gate>>) -> (String, String, MigrationManager) {
    let mut migration = MigrationConfig::new(dir);
    migration.backup_retention = None;
    migration.options = MigrationOptions {
        failpoint: Some(Arc::new(move |fp| {
            if let (Some(gate), Failpoint::DuringStage { thought: 0 }) = (&gate, fp) {
                gate.wait();
            }
            false
        })),
        progress: None,
    };
    let manager = MigrationManager::start(migration).unwrap();
    let tls = dir.join("tls");
    let config = MentisDbServerConfig {
        service: MentisDbServiceConfig::new(
            dir.to_path_buf(),
            "default",
            StorageAdapterKind::Binary,
        ),
        mcp_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        rest_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        https_mcp_addr: None,
        https_rest_addr: None,
        tls_cert_path: tls.join("cert.pem"),
        tls_key_path: tls.join("key.pem"),
        dashboard_addr: None,
        dashboard_pin: None,
    };
    let handles = start_servers_with_migrations(config, None, Some(manager.clone()))
        .await
        .expect("start servers");
    let rest_url = format!("http://{}", handles.rest.local_addr());
    let mcp_url = format!("http://{}", handles.mcp.local_addr());
    // Keep the servers alive for the test's lifetime.
    std::mem::forget(handles);
    (rest_url, mcp_url, manager)
}

async fn head(rest_url: &str, chain: &str) -> Reply {
    rest(
        "POST",
        format!("{rest_url}/v1/head"),
        Some(json!({ "chain_key": chain })),
    )
    .await
}

/// While a chain migrates it answers 503 with `Retry-After`, a percent and a
/// pointer to the status endpoint; every other chain is served; afterwards the
/// chain is served with all its thoughts.
#[tokio::test]
async fn test_migrating_chain_is_503_and_others_are_served() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "old", FixtureFormat::Schema1, 40);
    put(dir.path(), "current", FixtureFormat::Current, 7);
    let gate = Arc::new(Gate::default());
    let (rest_url, mcp_url, manager) = daemon(dir.path(), Some(Arc::clone(&gate))).await;

    let blocked = head(&rest_url, "old").await;
    assert_eq!(blocked.status, 503, "{:?}", blocked.body);
    assert_eq!(blocked.retry_after.as_deref(), Some("5"));
    assert_eq!(blocked.body["retry_after_secs"], 5);
    assert_eq!(blocked.body["migration"]["state"], "migrating");
    assert!(blocked.body["error"]
        .as_str()
        .unwrap()
        .contains("being migrated"));

    let served = head(&rest_url, "current").await;
    assert_eq!(served.status, 200, "{:?}", served.body);
    assert_eq!(served.body["thought_count"], 7);

    let status = rest("GET", format!("{rest_url}/v1/migrations"), None).await;
    assert_eq!(status.status, 200);
    assert_eq!(status.body["active"], true);
    let chains = status.body["chains"].as_array().unwrap();
    assert!(chains.iter().any(|c| c["chain_key"] == "old"));
    assert!(!chains.iter().any(|c| c["chain_key"] == "current"));

    // The MCP tool reports the same thing.
    let mcp = rest(
        "POST",
        format!("{mcp_url}/tools/execute"),
        Some(json!({"tool": "mentisdb_migration_status", "parameters": {}})),
    )
    .await;
    assert_eq!(mcp.status, 200, "{:?}", mcp.body);
    assert!(mcp.body.to_string().contains("\"old\""), "{:?}", mcp.body);

    gate.release();
    let manager2 = manager.clone();
    assert!(
        tokio::task::spawn_blocking(move || manager2.wait_idle(Duration::from_secs(60)))
            .await
            .unwrap()
    );
    let done = head(&rest_url, "old").await;
    assert_eq!(done.status, 200, "{:?}", done.body);
    assert_eq!(done.body["thought_count"], 40);
}

/// A corrupt chain is quarantined: 503 without `Retry-After`, a message naming
/// the fix, the other chains unaffected; `retry` over REST clears it.
#[tokio::test]
async fn test_quarantined_chain_is_503_without_retry_after_and_retry_works() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "bad", FixtureFormat::Schema3Base, 12);
    put(dir.path(), "fine", FixtureFormat::Schema1, 5);
    let path = dir
        .path()
        .join(chain_storage_filename("bad", StorageAdapterKind::Binary));
    let good = std::fs::read(&path).unwrap();
    let mut corrupt = good.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0x01;
    std::fs::write(&path, &corrupt).unwrap();

    let (rest_url, _mcp, manager) = daemon(dir.path(), None).await;
    let manager2 = manager.clone();
    assert!(
        tokio::task::spawn_blocking(move || manager2.wait_idle(Duration::from_secs(60)))
            .await
            .unwrap()
    );

    let bad = head(&rest_url, "bad").await;
    assert_eq!(bad.status, 503, "{:?}", bad.body);
    assert_eq!(bad.retry_after, None);
    assert_eq!(bad.body["migration"]["state"], "quarantined");
    assert!(bad.body["error"]
        .as_str()
        .unwrap()
        .contains("migrate retry --chain bad"));
    assert_eq!(head(&rest_url, "fine").await.status, 200);

    std::fs::write(&path, &good).unwrap();
    let retried = rest(
        "POST",
        format!("{rest_url}/v1/migrations/retry"),
        Some(json!({"chain_key": "bad"})),
    )
    .await;
    assert_eq!(retried.status, 200, "{:?}", retried.body);
    let manager3 = manager.clone();
    assert!(
        tokio::task::spawn_blocking(move || manager3.wait_idle(Duration::from_secs(60)))
            .await
            .unwrap()
    );
    let ok = head(&rest_url, "bad").await;
    assert_eq!(ok.status, 200, "{:?}", ok.body);
    assert_eq!(ok.body["thought_count"], 12);
}

/// A daemon without a manager keeps the library behaviour: it upgrades a
/// legacy chain in place when first opened and answers 200.
#[tokio::test]
async fn test_without_a_manager_a_legacy_chain_still_opens() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "legacy", FixtureFormat::Schema2, 9);
    let tls = dir.path().join("tls");
    let config = MentisDbServerConfig {
        service: MentisDbServiceConfig::new(
            dir.path().to_path_buf(),
            "default",
            StorageAdapterKind::Binary,
        ),
        mcp_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        rest_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        https_mcp_addr: None,
        https_rest_addr: None,
        tls_cert_path: tls.join("cert.pem"),
        tls_key_path: tls.join("key.pem"),
        dashboard_addr: None,
        dashboard_pin: None,
    };
    let handles = mentisdb::server::start_servers(config, None).await.unwrap();
    let rest_url = format!("http://{}", handles.rest.local_addr());
    let reply = head(&rest_url, "legacy").await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["thought_count"], 9);
    let status = rest("GET", format!("{rest_url}/v1/migrations"), None).await;
    assert_eq!(status.body["active"], false);
    assert_eq!(status.body["chains"], json!([]));
}

/// The dashboard must not open a chain the migration manager owns, and exposes
/// the same state for its banner.
#[tokio::test]
async fn test_dashboard_gate_and_banner_endpoint() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "old", FixtureFormat::Schema1, 20);
    let gate = Arc::new(Gate::default());
    let (rest_url, _mcp, manager) = daemon(dir.path(), Some(Arc::clone(&gate))).await;
    assert!(mentisdb::migration::manager_for_dir(dir.path()).is_some());

    // The dashboard is served over TLS only; exercise its handlers through the
    // public manager registry the way `api_migrations` does.
    let found = mentisdb::migration::manager_for_dir(dir.path()).unwrap();
    assert!(found.is_active());
    assert!(found.statuses().iter().any(|s| s.chain_key == "old"));
    assert!(matches!(
        found.gate("old"),
        Some(mentisdb::migration::MigrationBlock::Migrating { .. })
    ));
    gate.release();
    let manager2 = manager.clone();
    assert!(
        tokio::task::spawn_blocking(move || manager2.wait_idle(Duration::from_secs(60)))
            .await
            .unwrap()
    );
    assert_eq!(head(&rest_url, "old").await.status, 200);
}
