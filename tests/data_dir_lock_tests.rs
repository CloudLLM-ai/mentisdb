//! One writer per data directory: a second process on the same directory is
//! refused, naming the holder, and the lock dies with the holder.
//!
//! The holder is this test binary re-run as a child, so the lock is held by a
//! different process, exactly like a daemon. No port is bound.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use mentisdb::operator::DataDirLock;
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};

const ENV_DIR: &str = "MENTISDB_TEST_LOCK_HOLDER_DIR";

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mentisdb_lock_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Not a test on its own: when the parent sets the env var, this process takes
/// the lock, announces it, and waits to be killed.
#[test]
fn lock_holder_child() {
    let Ok(dir) = std::env::var(ENV_DIR) else {
        return;
    };
    let _lock = DataDirLock::claim(std::path::Path::new(&dir)).expect("child claims the lock");
    println!("READY {}", std::process::id());
    std::thread::sleep(std::time::Duration::from_secs(120));
}

fn spawn_holder(dir: &std::path::Path) -> (Child, u32) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "lock_holder_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_DIR, dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    for line in BufReader::new(stdout).lines() {
        let line = line.unwrap();
        // The harness prefixes the first output with "test <name> ... ".
        if let Some((_, pid)) = line.split_once("READY ") {
            return (child, pid.trim().parse().unwrap());
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("holder exited without taking the lock");
}

fn cli(args: &[&str]) -> (bool, String) {
    let argv = std::iter::once("mentisdb".to_string()).chain(args.iter().map(ToString::to_string));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = mentisdb::cli::run_with_io(
        argv,
        &mut std::io::Cursor::new(String::new()),
        &mut out,
        &mut err,
    );
    let ok = format!("{code:?}") == format!("{:?}", std::process::ExitCode::SUCCESS);
    (
        ok,
        format!(
            "{}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        ),
    )
}

/// A second claimant is refused with a message naming the pid and the directory.
#[test]
fn second_claim_is_refused_naming_the_holder() {
    let dir = dir("refuse");
    let (mut child, pid) = spawn_holder(&dir);
    let error = DataDirLock::claim(&dir).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    let message = error.to_string();
    assert!(message.contains(&format!("pid {pid}")), "{message}");
    assert!(message.contains(&dir.display().to_string()), "{message}");
    assert_eq!(DataDirLock::holder(&dir), Some(pid));
    child.kill().unwrap();
    child.wait().unwrap();
}

/// The OS drops the lock when the holder is killed: no stale lock file blocks
/// the next start.
#[test]
fn lock_is_released_when_the_holder_is_killed() {
    let dir = dir("release");
    let (mut child, _) = spawn_holder(&dir);
    assert!(DataDirLock::claim(&dir).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    let lock = DataDirLock::claim(&dir).expect("released after the holder died");
    assert_eq!(DataDirLock::holder(&dir), Some(std::process::id()));
    drop(lock);
}

/// `--offline` operator writes refuse while another process holds the
/// directory, and change nothing.
#[test]
fn offline_write_is_refused_while_a_daemon_holds_the_lock() {
    let dir = dir("offline");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append("alice", ThoughtType::Insight, "hello")
        .unwrap();
    chain.flush().unwrap();
    drop(chain);
    let data = dir.to_str().unwrap();
    let (mut child, pid) = spawn_holder(&dir);
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
    let (ok, text) = cli(&args);
    assert!(!ok, "{text}");
    assert!(text.contains(&format!("pid {pid}")), "{text}");
    assert!(
        !dir.join("operator").join("operator.key").exists(),
        "nothing written"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    let (ok, text) = cli(&args);
    assert!(ok, "after the holder exits: {text}");
}

/// Mutating migration commands refuse while a daemon holds the directory; the
/// read-only ones and dry runs still work.
#[test]
fn migrate_writes_are_refused_while_a_daemon_holds_the_lock() {
    let dir = dir("migrate");
    let data = dir.to_str().unwrap();
    let (mut child, pid) = spawn_holder(&dir);
    for args in [
        vec!["migrate", "run", "--dir", data],
        vec!["migrate", "retry", "--chain", "vault", "--dir", data],
        vec![
            "migrate", "skip", "--chain", "vault", "--reason", "x", "--dir", data,
        ],
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
        let (ok, text) = cli(&args);
        assert!(!ok, "{args:?} must refuse: {text}");
        assert!(text.contains(&format!("pid {pid}")), "{args:?}: {text}");
    }
    let (ok, text) = cli(&["migrate", "status", "--dir", data]);
    assert!(ok, "status is read-only: {text}");
    let (ok, text) = cli(&["migrate", "run", "--dry-run", "--dir", data]);
    assert!(ok, "dry run is read-only: {text}");
    child.kill().unwrap();
    child.wait().unwrap();
}
