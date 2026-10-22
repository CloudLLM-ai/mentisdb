//! The headless daemon must shut down gracefully on SIGTERM, the signal sent
//! by `docker stop`, Kubernetes and systemd.

#![cfg(all(unix, feature = "server"))]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `mentisdb --headless` started with no TTY stays up, then exits with status
/// 0 (not killed by signal 15) after receiving SIGTERM.
#[test]
fn test_headless_exits_cleanly_on_sigterm() {
    let dir = std::env::temp_dir().join(format!("mentisdb-sigterm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mentisdb");

    let stderr = child.stderr.take().expect("stderr piped");
    let mut lines = BufReader::new(stderr).lines();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let line = lines
            .next()
            .expect("daemon exited before announcing headless mode")
            .expect("read stderr");
        if line.contains("[mentisdb headless] MCP") {
            break;
        }
        assert!(Instant::now() < deadline, "daemon did not start in time");
    }

    // SAFETY: plain kill(2) on a child pid we own.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill(SIGTERM) failed");

    let wait_deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > wait_deadline {
            let _ = child.kill();
            panic!("daemon did not exit after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = std::fs::remove_dir_all(&dir);
    assert!(status.success(), "expected clean exit, got {status:?}");
}
