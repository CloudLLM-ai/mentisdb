#![cfg(feature = "server")]
//! `mentisdb agent key generate`: one command that creates an Ed25519 key pair,
//! saves the private seed owner-only and registers the public key for an agent.
//!
//! Each test serves the real dashboard router (PIN gated) on an ephemeral
//! loopback port over a temporary data directory, or applies the command with
//! `--offline`, and checks the two promises that matter: the seed file is the
//! only place the private key goes, and a failed registration never loses it.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use ed25519_dalek::SigningKey;
pub use mentisdb::access;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
pub use mentisdb::search;
use mentisdb::signing::to_hex;
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, ManagedVectorProviderKind, MentisDb, PublicKeyAlgorithm,
    RankedSearchGraph, RankedSearchHit, RankedSearchQuery, SkillFormat, SkillRegistry, SkillUpload,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use serde_json::Value;
use tokio::sync::RwLock;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

static COUNTER: AtomicU64 = AtomicU64::new(0);
const PIN: &str = "2468";

fn unique_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_keygen_{label}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A dashboard listener over one temporary data directory.
struct Daemon {
    dashboard: String,
    dir: PathBuf,
}

async fn start(pin: Option<&str>) -> Daemon {
    let dir = unique_dir("daemon");
    // The operator administers an existing chain; `reg` has one thought by `bot`.
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain.append("bot", ThoughtType::Insight, "hello").unwrap();
    chain.flush().unwrap();
    drop(chain);
    let state = dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dashboard = format!("http://{}", listener.local_addr().unwrap());
    let router = dashboard_impl::dashboard_router(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Daemon { dashboard, dir }
}

/// What one CLI invocation printed.
struct Outcome {
    ok: bool,
    out: String,
    err: String,
}

impl Outcome {
    fn json(&self) -> Value {
        assert!(self.ok, "command failed: {}", self.err);
        serde_json::from_str(&self.out).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.out))
    }
}

fn cli_blocking(args: Vec<String>) -> Outcome {
    let argv = std::iter::once("mentisdb".to_string()).chain(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code =
        mentisdb::cli::run_with_io(argv, &mut Cursor::new(String::new()), &mut out, &mut err);
    Outcome {
        ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

async fn cli(args: Vec<String>) -> Outcome {
    tokio::task::spawn_blocking(move || cli_blocking(args))
        .await
        .unwrap()
}

/// `mentisdb agent key generate <agent> --key-id <id> --chain reg --out <file>`
/// against the daemon, with the PIN unless `pin` is `None`.
async fn generate(
    d: &Daemon,
    key_id: &str,
    out: &Path,
    pin: Option<&str>,
    extra: &[&str],
) -> Outcome {
    let mut argv: Vec<String> = [
        "agent",
        "key",
        "generate",
        "bot",
        "--key-id",
        key_id,
        "--chain",
        "reg",
        "--out",
        out.to_str().unwrap(),
        "--dashboard-url",
        &d.dashboard,
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    if let Some(pin) = pin {
        argv.extend(["--pin".into(), pin.into()]);
    }
    argv.extend(extra.iter().map(ToString::to_string));
    cli(argv).await
}

fn seed_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap().trim().to_string()
}

fn public_of_seed(seed_hex: &str) -> String {
    let seed: [u8; 32] = mentisdb::signing::from_hex(seed_hex)
        .unwrap()
        .try_into()
        .unwrap();
    to_hex(&SigningKey::from_bytes(&seed).verifying_key().to_bytes())
}

/// The registered keys of `bot` on chain `reg`, read from disk.
fn registered_keys(dir: &Path) -> Vec<(String, String, bool)> {
    let chain =
        MentisDb::open_with_key_and_storage_kind(dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain
        .get_agent("bot")
        .map(|agent| {
            agent
                .public_keys
                .iter()
                .map(|k| {
                    (
                        k.key_id.clone(),
                        to_hex(&k.public_key_bytes),
                        k.revoked_at.is_some(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path).unwrap().permissions()) & 0o777
}

/// The command saves a 0600 seed file whose public key is the one registered,
/// prints that public key and the path, and never prints the seed, in JSON or text.
#[tokio::test(flavor = "multi_thread")]
async fn test_generate_saves_the_seed_and_registers_its_public_key() {
    let d = start(Some(PIN)).await;
    let files = unique_dir("files");
    let file = files.join("builder.key");

    let json = generate(&d, "builder-1", &file, Some(PIN), &["--json"]).await;
    let reply = json.json();
    let seed = seed_of(&file);
    assert_eq!(seed.len(), 64, "seed is 64 hex characters");
    let public = public_of_seed(&seed);
    assert_eq!(reply["public_key_hex"], public.as_str());
    assert_eq!(reply["key_id"], "builder-1");
    assert_eq!(reply["agent_id"], "bot");
    assert_eq!(reply["chain_key"], "reg");
    assert_eq!(reply["key_file"], file.to_str().unwrap());
    assert_eq!(reply["registered"], true);
    assert!(!json.out.contains(&seed) && !json.err.contains(&seed));
    assert!(!json.out.contains("seed") && !json.out.contains("private_key\""));
    #[cfg(unix)]
    assert_eq!(mode(&file), 0o600);
    assert_eq!(
        registered_keys(&d.dir),
        vec![("builder-1".to_string(), public.clone(), false)],
        "the public key of the saved seed is what was registered"
    );
    assert!(
        std::fs::read_to_string(&file).unwrap().ends_with('\n'),
        "the file is the hex and a newline, the format --private-key-file reads"
    );
    assert!(mentisdb::cli::remote::read_secret_key_file(file.to_str().unwrap()).is_ok());

    let text = generate(&d, "builder-2", &files.join("b2.key"), Some(PIN), &[]).await;
    assert!(text.ok, "{}", text.err);
    let seed2 = seed_of(&files.join("b2.key"));
    assert_ne!(seed2, seed, "every run makes a new key");
    for line in [
        "Registered key builder-2 for agent bot on chain reg",
        &format!("public_key_hex: {}", public_of_seed(&seed2)),
        "private key saved to ",
        "!! This file is the only copy of the private key.",
        "never paste it into a chat or give it to an AI agent.",
    ] {
        assert!(
            text.out.contains(line),
            "missing {line:?} in:\n{}",
            text.out
        );
    }
    assert!(!text.out.contains(&seed2) && !text.err.contains(&seed2));
    let _ = std::fs::remove_dir_all(&files);
}

/// A key id the agent already has is refused before any file is written, and the
/// registered key is not touched; revoked ids stay taken.
#[tokio::test(flavor = "multi_thread")]
async fn test_generate_refuses_a_taken_key_id_before_writing() {
    let d = start(Some(PIN)).await;
    let files = unique_dir("dup");
    let first = files.join("first.key");
    assert!(generate(&d, "k1", &first, Some(PIN), &[]).await.ok);
    let before = registered_keys(&d.dir);

    let second = files.join("second.key");
    let refused = generate(&d, "k1", &second, Some(PIN), &[]).await;
    assert!(!refused.ok);
    assert!(
        refused.err.contains("already has a key 'k1'") && refused.err.contains("No key file"),
        "{}",
        refused.err
    );
    assert!(!second.exists(), "nothing is written for a taken key id");
    assert_eq!(registered_keys(&d.dir), before);

    let revoke = cli([
        "agent",
        "key",
        "revoke",
        "bot",
        "--key-id",
        "k1",
        "--chain",
        "reg",
        "--pin",
        PIN,
        "--dashboard-url",
        &d.dashboard,
    ]
    .iter()
    .map(ToString::to_string)
    .collect())
    .await;
    assert!(revoke.ok, "{}", revoke.err);
    let after_revoke = generate(&d, "k1", &second, Some(PIN), &[]).await;
    assert!(!after_revoke.ok && !second.exists(), "{}", after_revoke.err);
    let _ = std::fs::remove_dir_all(&files);
}

/// `--out` refuses an existing file without `--force` and registers nothing;
/// `--force` replaces it and leaves it owner-only even if it was world-readable.
#[tokio::test(flavor = "multi_thread")]
async fn test_generate_out_and_force() {
    let d = start(Some(PIN)).await;
    let files = unique_dir("force");
    let file = files.join("existing.key");
    std::fs::write(&file, "keep me\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();

    let refused = generate(&d, "k1", &file, Some(PIN), &[]).await;
    assert!(
        !refused.ok && refused.err.contains("--force"),
        "{}",
        refused.err
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me\n");
    assert!(
        registered_keys(&d.dir).is_empty(),
        "an unwritable file registers nothing"
    );

    let forced = generate(&d, "k1", &file, Some(PIN), &["--force"]).await;
    assert!(forced.ok, "{}", forced.err);
    let seed = seed_of(&file);
    assert_eq!(seed.len(), 64);
    #[cfg(unix)]
    assert_eq!(mode(&file), 0o600);
    assert_eq!(registered_keys(&d.dir)[0].1, public_of_seed(&seed));
    let _ = std::fs::remove_dir_all(&files);
}

/// Registration that fails after the file is written (here: a daemon without a
/// PIN refuses operator commands) keeps the key file, exits non-zero and says how
/// to register it later, with the public key; the seed is not in the message.
#[tokio::test(flavor = "multi_thread")]
async fn test_failed_registration_keeps_the_file_and_prints_the_recovery_command() {
    let d = start(None).await;
    let files = unique_dir("recover");
    let file = files.join("kept.key");
    let failed = generate(&d, "k1", &file, None, &[]).await;
    assert!(!failed.ok, "{}", failed.out);
    let seed = seed_of(&file);
    let public = public_of_seed(&seed);
    assert!(
        failed.err.contains(&format!("saved at {}", file.display())),
        "{}",
        failed.err
    );
    assert!(
        failed.err.contains(&format!(
            "mentisdb agent key add bot --key-id k1 --chain reg --public-key-hex {public}"
        )),
        "{}",
        failed.err
    );
    assert!(!failed.err.contains(&seed) && !failed.out.contains(&seed));
    #[cfg(unix)]
    assert_eq!(mode(&file), 0o600);
    assert!(registered_keys(&d.dir).is_empty());

    // The printed command registers the saved file's public key (here offline,
    // as the operator on the host would).
    let registered = cli_blocking(
        [
            "agent",
            "key",
            "add",
            "bot",
            "--key-id",
            "k1",
            "--chain",
            "reg",
            "--public-key-hex",
            &public,
            "--offline",
            "--data-dir",
            d.dir.to_str().unwrap(),
        ]
        .iter()
        .map(ToString::to_string)
        .collect(),
    );
    assert!(registered.ok, "{}", registered.err);
    assert_eq!(registered_keys(&d.dir)[0].1, public);
    let _ = std::fs::remove_dir_all(&files);
}

/// A wrong PIN or an unreachable daemon fails before a key is made, so no file
/// is left behind and nothing is registered.
#[tokio::test(flavor = "multi_thread")]
async fn test_bad_pin_or_unreachable_daemon_writes_no_file() {
    let d = start(Some(PIN)).await;
    let files = unique_dir("badpin");
    let file = files.join("never.key");
    let wrong = generate(&d, "k1", &file, Some("0000"), &[]).await;
    assert!(
        !wrong.ok && wrong.err.contains("PIN missing or wrong"),
        "{}",
        wrong.err
    );
    assert!(!file.exists());
    let down = cli([
        "agent",
        "key",
        "generate",
        "bot",
        "--key-id",
        "k1",
        "--chain",
        "reg",
        "--out",
        file.to_str().unwrap(),
        "--dashboard-url",
        "http://127.0.0.1:9",
        "--pin",
        PIN,
    ]
    .iter()
    .map(ToString::to_string)
    .collect())
    .await;
    assert!(
        !down.ok && down.err.contains("cannot reach"),
        "{}",
        down.err
    );
    assert!(!file.exists());
    assert!(registered_keys(&d.dir).is_empty());
    let _ = std::fs::remove_dir_all(&files);
}

/// `--offline` registers straight into the data directory, refuses a taken key
/// id without writing a file, and without `--out` saves under `$MENTISDB_KEY_DIR`.
#[test]
fn test_generate_offline_and_default_key_dir() {
    static KEY_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = KEY_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let data = unique_dir("offline");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&data, "reg", StorageAdapterKind::Binary).unwrap();
    chain.append("bot", ThoughtType::Insight, "hello").unwrap();
    chain.flush().unwrap();
    drop(chain);
    let keys = unique_dir("keydir").join("keys");
    std::env::set_var("MENTISDB_KEY_DIR", &keys);
    let run = |key_id: &str| {
        cli_blocking(
            [
                "agent",
                "key",
                "generate",
                "bot",
                "--key-id",
                key_id,
                "--chain",
                "reg",
                "--offline",
                "--data-dir",
                data.to_str().unwrap(),
                "--json",
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
        )
    };
    let first = run("off-1");
    let again = run("off-1");
    std::env::remove_var("MENTISDB_KEY_DIR");

    let reply = first.json();
    let public = reply["public_key_hex"].as_str().unwrap();
    let file = keys.join(format!("{}.key", &public[..16]));
    assert_eq!(reply["key_file"], file.to_str().unwrap());
    assert_eq!(public_of_seed(&seed_of(&file)), public);
    assert_eq!(registered_keys(&data)[0].1, public);
    #[cfg(unix)]
    {
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&keys), 0o700);
    }
    assert!(
        !again.ok && again.err.contains("already has a key 'off-1'"),
        "{}",
        again.err
    );
    assert_eq!(
        std::fs::read_dir(&keys).unwrap().count(),
        1,
        "the refused run wrote no second file"
    );
}
