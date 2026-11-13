#![cfg(feature = "server")]

//! The `mentisdb` binary must not start on a `.env` it cannot fully load: the
//! parser stops at the first bad line, which can drop the PIN or the
//! bearer-token switch and leave the daemon open without a word.
//!
//! Neither case binds a port: the daemon exits before it opens a listener,
//! and `--help` never starts one (see [`run`] for the guard rails).

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run the daemon in `dir` and collect its output. Every port is `0`
/// (ephemeral), so a daemon that wrongly starts never touches 9471-9475, and
/// it is killed after 30 s so a regression fails instead of hanging.
fn run(dir: &std::path::Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mentisdb"));
    command
        .args(args)
        .current_dir(dir)
        .env("MENTISDB_DIR", dir)
        .env("HOME", dir)
        .env_remove("MENTISDB_DASHBOARD_PIN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for port in [
        "MENTISDB_MCP_PORT",
        "MENTISDB_REST_PORT",
        "MENTISDB_HTTPS_MCP_PORT",
        "MENTISDB_HTTPS_REST_PORT",
        "MENTISDB_DASHBOARD_PORT",
    ] {
        command.env(port, "0");
    }
    let mut child = command.spawn().expect("spawn mentisdb");
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("poll mentisdb").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("mentisdb did not exit: it started despite the bad .env");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collect output")
}

/// A bad line ahead of the PIN: exit status 2, the file and line named on
/// stderr, and the PIN's value nowhere in the output.
#[test]
fn test_daemon_refuses_to_start_on_a_malformed_env_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "MENTISDB_UPDATE_CHECK=false\nB=x y\nMENTISDB_DASHBOARD_PIN=hunter2-pin\n",
    )
    .unwrap();

    let out = run(dir.path(), &[]);

    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".env"), "{stderr}");
    assert!(stderr.contains("line 2"), "{stderr}");
    assert!(stderr.contains("refusing to start"), "{stderr}");
    assert!(!stderr.contains("hunter2-pin"), "{stderr}");
}

/// `--help` still works with the same file: an operator must be able to read
/// the help while fixing it. It warns instead of refusing.
#[test]
fn test_help_still_works_with_a_malformed_env_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "B=x y\n").unwrap();

    let out = run(dir.path(), &["--help"]);

    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning:"));
}
