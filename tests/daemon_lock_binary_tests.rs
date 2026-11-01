#![cfg(feature = "server")]
//! The built `mentisdb` binary, launched for real against a temporary data
//! directory: one daemon per directory, the lock dies with the process, and the
//! offline and migration writers refuse while a daemon runs.
//!
//! Every daemon here is headless, bound to loopback with the MCP and REST ports
//! set to 0 (the OS picks free ephemeral ports) and the HTTPS and dashboard
//! listeners disabled. Nothing touches ports 9471-9475 or the user's own data
//! directory, and `HOME` points at the scratch directory too.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mentisdb_daemon_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mentisdb"));
    cmd.env("MENTISDB_DIR", dir)
        .env("HOME", dir)
        .env("MENTISDB_BIND_HOST", "127.0.0.1")
        .env("MENTISDB_MCP_PORT", "0")
        .env("MENTISDB_REST_PORT", "0")
        .env("MENTISDB_HTTPS_MCP_PORT", "0")
        .env("MENTISDB_HTTPS_REST_PORT", "0")
        .env("MENTISDB_DASHBOARD_PORT", "0")
        .env_remove("MENTISDB_BEARER_TOKEN")
        .stdin(Stdio::null());
    cmd
}

/// Start a headless daemon and wait until it reports its listeners.
fn start_daemon(dir: &Path) -> Child {
    let mut child = command(dir)
        .arg("--headless")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("[mentisdb headless] MCP http://") {
                let _ = tx.send(());
                // Keep draining so the daemon never blocks on a full pipe.
            }
        }
    });
    if rx.recv_timeout(Duration::from_secs(60)).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("daemon did not become ready");
    }
    child
}

fn run(dir: &Path, args: &[&str]) -> Output {
    command(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn kill(mut child: Child) {
    child.kill().unwrap(); // SIGKILL on Unix: no cleanup code runs.
    child.wait().unwrap();
}

fn seed_chain(dir: &Path) {
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "hello")
        .unwrap();
    chain.flush().unwrap();
}

/// A second daemon on a directory in use exits nonzero and says who has it.
#[test]
fn second_daemon_on_the_same_directory_is_refused() {
    let dir = scratch("second");
    let first = start_daemon(&dir);
    let started = Instant::now();
    let second = run(&dir, &["--headless"]);
    assert!(!second.status.success(), "{}", text(&second));
    let message = text(&second);
    assert!(
        message.contains(&format!(
            "another mentisdb process (pid {}) is using {}",
            first.id(),
            dir.display()
        )),
        "{message}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "refusal is immediate"
    );
    kill(first);
}

/// SIGKILL runs no cleanup, yet a new daemon starts at once: the OS released
/// the lock, and no stale lock file blocks it.
#[test]
fn lock_is_released_when_the_daemon_is_killed() {
    let dir = scratch("kill");
    let first = start_daemon(&dir);
    assert!(dir.join("mentisdb.lock").exists());
    kill(first);
    let second = start_daemon(&dir);
    kill(second);
}

/// Offline operator commands refuse while the daemon runs, and work after.
#[test]
fn offline_operator_command_is_refused_while_the_daemon_runs() {
    let dir = scratch("offline");
    seed_chain(&dir);
    let data = dir.to_str().unwrap();
    let args = [
        "chain",
        "access",
        "set",
        "private",
        "--chain",
        "vault",
        "--offline",
        "--data-dir",
        data,
    ];
    let daemon = start_daemon(&dir);
    let refused = run(&dir, &args);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert!(
        text(&refused).contains(&format!("pid {}", daemon.id())),
        "{}",
        text(&refused)
    );
    assert!(
        !dir.join("operator").join("operator.key").exists(),
        "nothing written"
    );
    kill(daemon);
    let applied = run(&dir, &args);
    assert!(applied.status.success(), "{}", text(&applied));
}

/// The migrate CLI cannot write while the daemon owns the directory; its
/// read-only forms still work.
#[test]
fn migrate_cannot_write_while_the_daemon_runs() {
    let dir = scratch("migrate");
    seed_chain(&dir);
    let data = dir.to_str().unwrap();
    let daemon = start_daemon(&dir);
    for args in [
        vec!["migrate", "run", "--dir", data],
        vec!["migrate", "retry", "--chain", "vault", "--dir", data],
        vec!["migrate", "skip", "--chain", "vault", "--dir", data],
        vec!["migrate", "rollback", "--chain", "vault", "--dir", data],
        vec![
            "migrate",
            "purge-backups",
            "--older-than-days",
            "1",
            "--dir",
            data,
        ],
    ] {
        let out = run(&dir, &args);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
        assert!(
            text(&out).contains(&format!("pid {}", daemon.id())),
            "{args:?}: {}",
            text(&out)
        );
    }
    let status = run(&dir, &["migrate", "status", "--dir", data]);
    assert!(status.status.success(), "{}", text(&status));
    let dry = run(&dir, &["migrate", "run", "--dry-run", "--dir", data]);
    assert!(dry.status.success(), "{}", text(&dry));
    kill(daemon);
    let after = run(&dir, &["migrate", "run", "--dir", data]);
    assert!(after.status.success(), "{}", text(&after));
}
