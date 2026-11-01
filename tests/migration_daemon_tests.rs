//! The real `mentisdb --headless` binary must come up and serve while a large
//! legacy chain migrates in the background, and must never block startup on it.

#![cfg(all(unix, feature = "server"))]

use mentisdb::migration::fixtures::{write_large_fixture, FixtureFormat};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn post(url: &str, body: Value) -> (u16, Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("content-type", "application/json")
        .send(body.to_string())
        .expect("request");
    (
        response.status().as_u16(),
        response.body_mut().read_json().unwrap_or(Value::Null),
    )
}

fn get(url: &str) -> (u16, Value) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent.get(url).call().expect("request");
    (
        response.status().as_u16(),
        response.body_mut().read_json().unwrap_or(Value::Null),
    )
}

/// Chain size; override with `MENTISDB_MIGRATION_TEST_THOUGHTS`.
fn thoughts() -> usize {
    std::env::var("MENTISDB_MIGRATION_TEST_THOUGHTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600_000)
}

/// With a large schema-1 chain in the data directory the daemon announces
/// itself quickly, reports the migration as active, answers the chain with 503,
/// then serves it with every thought once the background migration finishes.
#[test]
fn test_daemon_serves_while_a_large_chain_migrates() {
    let count = thoughts();
    let dir = std::env::temp_dir().join(format!("mentisdb-daemon-migrate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    write_large_fixture(
        &dir.join(chain_storage_filename("big", StorageAdapterKind::Binary)),
        FixtureFormat::Schema1,
        count,
    )
    .unwrap();

    let spawned = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mentisdb"))
        .arg("--headless")
        .env("MENTISDB_DIR", &dir)
        .env("MENTISDB_BIND_HOST", "127.0.0.1")
        .env("MENTISDB_MCP_PORT", "0")
        .env("MENTISDB_REST_PORT", "0")
        .env("MENTISDB_HTTPS_MCP_PORT", "0")
        .env("MENTISDB_HTTPS_REST_PORT", "0")
        .env("MENTISDB_DASHBOARD_PORT", "0")
        .env("MENTISDB_STARTUP_SOUND", "false")
        .env("MENTISDB_UPDATE_CHECK", "false")
        .env("MENTISDB_MIGRATION_BACKUP_RETENTION_DAYS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mentisdb");
    let stderr = child.stderr.take().expect("stderr piped");
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(60);
    let rest = loop {
        let line = lines
            .next()
            .expect("daemon exited before announcing")
            .expect("read stderr");
        if let Some(rest) = line
            .split("REST ")
            .nth(1)
            .filter(|_| line.contains("[mentisdb headless] MCP"))
        {
            break rest.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "daemon did not start in time");
    };
    let startup = spawned.elapsed();
    // Keep draining stderr so the daemon never blocks on a full pipe.
    std::thread::spawn(move || for _ in lines {});

    assert_eq!(get(&format!("{rest}/health")).0, 200);
    let (_, status) = get(&format!("{rest}/v1/migrations"));
    assert_eq!(
        status["active"], true,
        "migration should still be running: {status}"
    );
    let (code, blocked) = post(&format!("{rest}/v1/head"), json!({"chain_key": "big"}));
    assert_eq!(code, 503, "{blocked}");
    assert_eq!(blocked["migration"]["state"], "migrating");

    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let (code, body) = post(&format!("{rest}/v1/head"), json!({"chain_key": "big"}));
        if code == 200 {
            assert_eq!(body["thought_count"].as_u64(), Some(count as u64));
            break;
        }
        assert_eq!(code, 503, "{body}");
        assert!(Instant::now() < deadline, "migration never finished");
        std::thread::sleep(Duration::from_millis(200));
    }
    let total = spawned.elapsed();
    println!(
        "DAEMON startup (announce) {startup:?}; chain of {count} served after {total:?} \
         (migrated in the background)"
    );
    assert!(
        startup < total / 2,
        "startup ({startup:?}) must not include the migration ({total:?})"
    );

    // SAFETY: plain kill(2) on a child pid we own.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
