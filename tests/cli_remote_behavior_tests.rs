#![cfg(feature = "server")]
//! End-to-end tests for the daemon-facing CLI commands: each test serves the
//! real REST (and dashboard) router on an ephemeral loopback port over a
//! temporary data directory and drives `mentisdb::cli::run_with_io` against it.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
pub use mentisdb::access;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
pub use mentisdb::search;
pub use mentisdb::server::{rest_router, MentisDbServiceConfig};
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

fn unique_dir() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("mentisdb_cli_behavior_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A REST and a dashboard listener over one temporary data directory.
struct Daemon {
    rest: String,
    dashboard: String,
    dir: PathBuf,
}

async fn serve(router: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    url
}

async fn start(pin: Option<&str>) -> Daemon {
    let dir = unique_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let config = MentisDbServiceConfig::new(dir.clone(), "default", StorageAdapterKind::Binary);
    let rest = serve(rest_router(config)).await;
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
    let dashboard = serve(dashboard_impl::dashboard_router(state)).await;
    Daemon {
        rest,
        dashboard,
        dir,
    }
}

/// A fresh REST listener over the same data directory.
///
/// In the daemon the dashboard and REST share one in-memory chain map; in these
/// tests they do not, so after an operator change made through the dashboard a
/// new listener is what picks it up from disk.
async fn reopen_rest(daemon: &Daemon) -> Daemon {
    let config =
        MentisDbServiceConfig::new(daemon.dir.clone(), "default", StorageAdapterKind::Binary);
    Daemon {
        rest: serve(rest_router(config)).await,
        dashboard: daemon.dashboard.clone(),
        dir: daemon.dir.clone(),
    }
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

async fn cli(args: Vec<String>, stdin: &str) -> Outcome {
    let stdin = stdin.to_string();
    tokio::task::spawn_blocking(move || {
        let argv = std::iter::once("mentisdb".to_string()).chain(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = mentisdb::cli::run_with_io(argv, &mut Cursor::new(stdin), &mut out, &mut err);
        Outcome {
            ok: format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS),
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    })
    .await
    .unwrap()
}

/// Run `mentisdb <args> --url <rest> --dashboard-url <dashboard> --json`.
async fn run(daemon: &Daemon, args: &[&str]) -> Outcome {
    let mut argv: Vec<String> = args.iter().map(ToString::to_string).collect();
    argv.extend([
        "--url".into(),
        daemon.rest.clone(),
        "--dashboard-url".into(),
        daemon.dashboard.clone(),
        "--json".into(),
    ]);
    cli(argv, "").await
}

/// Bootstrap, append, read back, search and verify the chain through the CLI.
#[tokio::test(flavor = "multi_thread")]
async fn test_thought_lifecycle_round_trips_through_the_daemon() {
    let d = start(None).await;
    let boot = run(
        &d,
        &["chain", "bootstrap", "genesis note", "--chain", "life"],
    )
    .await
    .json();
    assert_eq!(boot["bootstrapped"], true);

    let appended = run(
        &d,
        &[
            "thought",
            "append",
            "Use UTC everywhere",
            "--chain",
            "life",
            "--type",
            "decision",
            "--tag",
            "time",
            "--importance",
            "0.9",
        ],
    )
    .await
    .json();
    assert_eq!(appended["thought"]["thought_type"], "Decision");
    assert_eq!(appended["thought"]["tags"], serde_json::json!(["time"]));

    let got = run(&d, &["thought", "get", "--index", "1", "--chain", "life"])
        .await
        .json();
    assert_eq!(got["thought"]["content"], "Use UTC everywhere");

    let genesis = run(&d, &["thought", "genesis", "--chain", "life"])
        .await
        .json();
    assert_eq!(genesis["thought"]["content"], "genesis note");

    let ranked = run(&d, &["search", "UTC", "--chain", "life"]).await.json();
    assert_eq!(
        ranked["results"][0]["thought"]["content"],
        "Use UTC everywhere"
    );
    let lexical = run(&d, &["search", "lexical", "UTC", "--chain", "life"])
        .await
        .json();
    assert_eq!(
        lexical["results"][0]["thought"]["content"],
        "Use UTC everywhere"
    );
    assert_eq!(lexical["results"][0]["matched_terms"][0], "utc");
    let filtered = run(
        &d,
        &["search", "filter", "--type", "Decision", "--chain", "life"],
    )
    .await
    .json();
    assert_eq!(filtered["thoughts"].as_array().unwrap().len(), 1);

    let head = run(&d, &["chain", "head", "--chain", "life"]).await.json();
    assert_eq!(head["thought_count"], 2);
    assert_eq!(head["integrity_ok"], true);
    let chains = run(&d, &["chain", "list"]).await.json();
    assert!(chains["chain_keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k == "life"));

    let walked = run(
        &d,
        &[
            "thought",
            "traverse",
            "--from",
            "head",
            "--direction",
            "backward",
            "--include-anchor",
            "true",
            "--chain",
            "life",
        ],
    )
    .await
    .json();
    assert_eq!(walked["thoughts"][0]["content"], "Use UTC everywhere");
}

/// `-` reads the thought text from standard input.
#[tokio::test(flavor = "multi_thread")]
async fn test_content_can_come_from_standard_input() {
    let d = start(None).await;
    let argv = vec![
        "thought".into(),
        "append".into(),
        "-".into(),
        "--chain".into(),
        "stdin".into(),
        "--url".into(),
        d.rest.clone(),
        "--json".into(),
    ];
    let out = cli(argv, "piped text\n").await.json();
    assert_eq!(out["thought"]["content"], "piped text\n");
}

/// The recent-context prompt and the memory export print raw text, not JSON.
#[tokio::test(flavor = "multi_thread")]
async fn test_document_commands_print_raw_text() {
    let d = start(None).await;
    run(
        &d,
        &["thought", "append", "remember the milk", "--chain", "docs"],
    )
    .await
    .json();
    let plain = |args: &[&str]| {
        let mut argv: Vec<String> = args.iter().map(ToString::to_string).collect();
        argv.extend(["--url".into(), d.rest.clone()]);
        cli(argv, "")
    };
    let recent = plain(&["thought", "recent", "--chain", "docs"]).await;
    assert!(
        recent.ok && recent.out.contains("=== RECENT CONTEXT ==="),
        "{}",
        recent.out
    );
    run(
        &d,
        &[
            "thought",
            "append",
            "never deploy on fridays",
            "--type",
            "Constraint",
            "--chain",
            "docs",
        ],
    )
    .await
    .json();
    let standing = plain(&["thought", "standing", "--chain", "docs"]).await;
    assert!(
        standing.ok
            && standing.out.contains("=== STANDING CONTEXT ===")
            && standing.out.contains("never deploy on fridays"),
        "{}",
        standing.out
    );
    let export = plain(&["memory", "export", "--chain", "docs"]).await;
    assert!(
        export.ok && export.out.starts_with("# MEMORY"),
        "{}",
        export.out
    );
    assert!(export.out.contains("remember the milk"));
    let envelope = run(&d, &["memory", "export", "--chain", "docs"])
        .await
        .json();
    assert!(envelope["markdown"]
        .as_str()
        .unwrap()
        .contains("remember the milk"));
}

/// The agent registry commands change the same record the registry returns.
#[tokio::test(flavor = "multi_thread")]
async fn test_agent_registry_commands() {
    let d = start(Some("2468")).await;
    run(
        &d,
        &[
            "thought", "append", "hi", "--agent", "bot", "--chain", "reg",
        ],
    )
    .await
    .json();
    let upserted = run(
        &d,
        &[
            "agent",
            "upsert",
            "bot",
            "--name",
            "Build Bot",
            "--owner",
            "ci",
            "--chain",
            "reg",
        ],
    )
    .await
    .json();
    assert_eq!(upserted["agent"]["display_name"], "Build Bot");
    run(
        &d,
        &[
            "agent",
            "describe",
            "bot",
            "Runs the builds",
            "--chain",
            "reg",
        ],
    )
    .await
    .json();
    run(&d, &["agent", "alias", "bot", "builder", "--chain", "reg"])
        .await
        .json();
    // Key and status administration is the operator's: it goes through the
    // dashboard (PIN) and changes the registry on disk.
    let key_hex = public_hex(0x11);
    let with_key = run(
        &d,
        &[
            "agent",
            "key",
            "add",
            "bot",
            "--key-id",
            "k1",
            "--public-key-hex",
            &key_hex,
            "--chain",
            "reg",
            "--pin",
            "2468",
        ],
    )
    .await
    .json();
    assert_eq!(with_key["changed"], true);
    let registry_on_disk = || {
        MentisDb::open_with_key_and_storage_kind(&d.dir, "reg", StorageAdapterKind::Binary)
            .unwrap()
            .agent_registry()
            .agents["bot"]
            .clone()
    };
    assert_eq!(registry_on_disk().public_keys[0].key_id, "k1");
    run(
        &d,
        &[
            "agent", "key", "revoke", "bot", "--key-id", "k1", "--chain", "reg", "--pin", "2468",
        ],
    )
    .await
    .json();
    assert!(registry_on_disk().public_keys[0].revoked_at.is_some());
    run(
        &d,
        &["agent", "disable", "bot", "--chain", "reg", "--pin", "2468"],
    )
    .await
    .json();
    assert_eq!(registry_on_disk().status, AgentStatus::Revoked);
    run(
        &d,
        &[
            "agent", "activate", "bot", "--chain", "reg", "--pin", "2468",
        ],
    )
    .await
    .json();
    assert_eq!(registry_on_disk().status, AgentStatus::Active);
    // Without the PIN the dashboard refuses.
    let refused = run(&d, &["agent", "disable", "bot", "--chain", "reg"]).await;
    assert!(!refused.ok, "{}", refused.out);
    let record = run(&d, &["agent", "get", "bot", "--chain", "reg"])
        .await
        .json();
    assert_eq!(record["agent"]["description"], "Runs the builds");
    assert!(record["agent"]["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "builder"));
    let listed = run(&d, &["agents", "--chain", "reg"]).await.json();
    assert!(listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["agent_id"] == "bot"));
    let registry = run(&d, &["agent", "registry", "--chain", "reg"])
        .await
        .json();
    assert!(!registry["agents"].as_array().unwrap().is_empty());
}

/// Entity types, branches and merges go through the same handlers as MCP.
#[tokio::test(flavor = "multi_thread")]
async fn test_entity_type_branch_and_merge() {
    let d = start(None).await;
    let first = run(&d, &["thought", "append", "base", "--chain", "main"])
        .await
        .json();
    let id = first["thought"]["id"].as_str().unwrap().to_string();
    let entity = run(
        &d,
        &["entity-type", "upsert", "customer", "--chain", "main"],
    )
    .await
    .json();
    assert_eq!(entity["entity_type"]["entity_type"], "customer");
    assert_eq!(
        run(&d, &["entity-type", "list", "--chain", "main"])
            .await
            .json()["entity_types"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let branch = run(
        &d,
        &[
            "chain", "branch", "--from", "main", "--at", &id, "--name", "side",
        ],
    )
    .await
    .json();
    assert_eq!(branch["branch_chain_key"], "side");
    run(&d, &["thought", "append", "on the side", "--chain", "side"])
        .await
        .json();

    let refused = run(&d, &["chain", "merge", "--from", "side", "--into", "main"]).await;
    assert!(
        !refused.ok && refused.err.contains("--yes"),
        "{}",
        refused.err
    );
    let merged = run(
        &d,
        &[
            "chain", "merge", "--from", "side", "--into", "main", "--yes",
        ],
    )
    .await
    .json();
    assert_eq!(merged["source_deleted"], true);
    assert!(merged["thoughts_copied"].as_u64().unwrap() >= 1);
}

/// Skills upload from a file, read back as raw text, and move through their lifecycle.
#[tokio::test(flavor = "multi_thread")]
async fn test_skill_commands() {
    let d = start(None).await;
    run(
        &d,
        &["thought", "append", "hi", "--agent", "bot", "--chain", "sk"],
    )
    .await
    .json();
    let file = unique_dir().with_extension("md");
    std::fs::write(
        &file,
        "---\nname: demo\ndescription: a demo\ntags: [x]\n---\n# Demo\nhello\n",
    )
    .unwrap();
    let file = file.to_str().unwrap();
    let up = run(
        &d,
        &[
            "skill", "upload", file, "--agent", "bot", "--id", "demo", "--chain", "sk",
        ],
    )
    .await
    .json();
    assert_eq!(up["skill"]["skill_id"], "demo");
    assert_eq!(
        run(&d, &["skill", "list", "--chain", "sk"]).await.json()["skills"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run(&d, &["skill", "search", "demo", "--chain", "sk"])
            .await
            .json()["skills"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        run(&d, &["skill", "versions", "demo", "--chain", "sk"])
            .await
            .json()["versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let plain = cli(
        vec![
            "skill".into(),
            "read".into(),
            "demo".into(),
            "--chain".into(),
            "sk".into(),
            "--url".into(),
            d.rest.clone(),
        ],
        "",
    )
    .await;
    assert!(
        plain.ok && plain.out.contains("hello") && !plain.out.trim_start().starts_with('{'),
        "{}",
        plain.out
    );
    let guide = cli(
        vec![
            "skill".into(),
            "guide".into(),
            "--url".into(),
            d.rest.clone(),
        ],
        "",
    )
    .await;
    assert!(guide.ok && guide.out.contains("MentisDB"));
    assert_eq!(
        run(
            &d,
            &[
                "skill",
                "deprecate",
                "demo",
                "--reason",
                "old",
                "--chain",
                "sk"
            ]
        )
        .await
        .json()["skill"]["status"],
        "deprecated"
    );
    assert_eq!(
        run(
            &d,
            &["skill", "revoke", "demo", "--reason", "bad", "--chain", "sk"]
        )
        .await
        .json()["skill"]["status"],
        "revoked"
    );
    run(&d, &["skill", "delete", "demo", "--yes", "--chain", "sk"])
        .await
        .json();
    assert!(
        run(&d, &["skill", "list", "--chain", "sk"]).await.json()["skills"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Daemon errors surface the server's message and a non-zero exit code.
#[tokio::test(flavor = "multi_thread")]
async fn test_failures_are_reported_with_the_server_message() {
    let d = start(None).await;
    run(&d, &["thought", "append", "x", "--chain", "err"])
        .await
        .json();
    let missing = run(&d, &["thought", "get", "--index", "99", "--chain", "err"]).await;
    assert!(!missing.ok);
    assert!(
        missing.err.starts_with("thought get failed: HTTP"),
        "{}",
        missing.err
    );

    let down = cli(
        vec![
            "daemon".into(),
            "health".into(),
            "--url".into(),
            "http://127.0.0.1:9".into(),
        ],
        "",
    )
    .await;
    assert!(
        !down.ok && down.err.contains("cannot reach http://127.0.0.1:9/health"),
        "{}",
        down.err
    );
}

/// `daemon health` and `daemon flush` hit the REST-only routes.
#[tokio::test(flavor = "multi_thread")]
async fn test_daemon_health_and_flush() {
    let d = start(None).await;
    assert_eq!(run(&d, &["daemon", "health"]).await.json()["status"], "ok");
    assert_eq!(
        run(&d, &["daemon", "flush"]).await.json()["status"],
        "flushed"
    );
}

/// Dashboard commands present the PIN and are refused without it.
#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_commands_need_the_pin() {
    let d = start(Some("1234")).await;
    let denied = run(&d, &["daemon", "version"]).await;
    assert!(
        !denied.ok && denied.err.contains("PIN missing or wrong"),
        "{}",
        denied.err
    );
    let wrong = run(&d, &["daemon", "version", "--pin", "0000"]).await;
    assert!(
        !wrong.ok && wrong.err.contains("PIN missing or wrong"),
        "{}",
        wrong.err
    );
    let ok = run(&d, &["daemon", "version", "--pin", "1234"])
        .await
        .json();
    assert_eq!(ok["version"], env!("CARGO_PKG_VERSION"));
}

/// Chain detail, agent copy and chain deletion work through the dashboard API.
#[tokio::test(flavor = "multi_thread")]
async fn test_dashboard_chain_and_agent_commands() {
    let d = start(None).await;
    // The dashboard and REST listeners share the data directory but not the
    // in-memory chain cache, so write through REST and flush to disk first.
    run(
        &d,
        &[
            "thought", "append", "one", "--agent", "bot", "--chain", "dash",
        ],
    )
    .await
    .json();
    run(&d, &["daemon", "flush"]).await.json();
    let info = run(&d, &["chain", "info", "dash"]).await.json();
    assert_eq!(info["chain_key"], "dash");
    assert_eq!(info["thought_count"], 1);
    let copied = run(
        &d,
        &[
            "agent",
            "copy",
            "bot",
            "--from",
            "dash",
            "--to",
            "dash-copy",
        ],
    )
    .await
    .json();
    assert_eq!(copied["copied"], 1);
    let refused = run(&d, &["chain", "delete", "dash-copy"]).await;
    assert!(!refused.ok && refused.err.contains("--yes"));
    let deleted = run(&d, &["chain", "delete", "dash-copy", "--yes"])
        .await
        .json();
    assert_eq!(deleted["deleted"], true);
    let settings = run(&d, &["daemon", "settings", "list"]).await.json();
    assert!(settings
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["name"] == "MENTISDB_DIR"));
}

fn write_seed(seed_byte: &str) -> String {
    let file = unique_dir().with_extension("key");
    std::fs::write(&file, format!("{}\n", seed_byte.repeat(32))).unwrap();
    file.to_str().unwrap().to_string()
}

fn public_hex(seed_byte: u8) -> String {
    mentisdb::signing::to_hex(
        &ed25519_dalek::SigningKey::from_bytes(&[seed_byte; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// Full private-chain workflow through the CLI: the operator makes a chain
/// private and manages its keys through the dashboard, agents sign and read
/// locally, and an allowed key stops working the moment it is removed.
#[tokio::test(flavor = "multi_thread")]
async fn test_chain_access_workflow_through_the_cli() {
    let d = start(Some("2468")).await;
    {
        // The chain exists (with one public thought) before the operator acts;
        // it is created on disk so the REST listener first opens it afterwards.
        let mut chain =
            MentisDb::open_with_key_and_storage_kind(&d.dir, "vault", StorageAdapterKind::Binary)
                .unwrap();
        chain
            .append("seed", ThoughtType::Insight, "opening note")
            .unwrap();
        chain.flush().unwrap();
    }
    let (admin_key, friend_key) = (write_seed("01"), write_seed("02"));
    fn op<'a>(args: &[&'a str]) -> Vec<&'a str> {
        let mut all: Vec<&'a str> = args.to_vec();
        all.extend(["--pin", "2468"]);
        all
    }
    for (agent, key_id, seed) in [("admin", "a1", 1), ("friend", "f1", 2)] {
        run(
            &d,
            &op(&[
                "agent",
                "key",
                "add",
                agent,
                "--key-id",
                key_id,
                "--public-key-hex",
                &public_hex(seed),
                "--chain",
                "vault",
            ]),
        )
        .await
        .json();
    }

    // Without a policy the chain is public.
    let shown = run(&d, &op(&["chain", "access", "admin", "--chain", "vault"]))
        .await
        .json();
    assert_eq!(shown["policy"]["mode"], "open");
    // allow/deny need an existing policy.
    let early = run(
        &d,
        &op(&[
            "chain",
            "access",
            "allow",
            "friend",
            "f1",
            &public_hex(2),
            "--chain",
            "vault",
        ]),
    )
    .await;
    assert!(
        !early.ok && early.err.contains("no access policy yet"),
        "{}",
        early.err
    );

    // The operator makes the chain private and allows the administrator.
    let set = run(
        &d,
        &op(&["chain", "access", "set", "private", "--chain", "vault"]),
    )
    .await
    .json();
    assert_eq!(set["policy"]["mode"], "allowlist");
    run(
        &d,
        &op(&[
            "chain",
            "access",
            "allow",
            "admin",
            "a1",
            &public_hex(1),
            "--chain",
            "vault",
        ]),
    )
    .await
    .json();

    // Unsigned appends are refused; the signed one lands.
    let refused = run(
        &d,
        &[
            "thought", "append", "no key", "--chain", "vault", "--agent", "admin",
        ],
    )
    .await;
    assert!(
        !refused.ok && refused.err.contains("signature"),
        "{}",
        refused.err
    );
    let signed = run(
        &d,
        &[
            "thought",
            "append",
            "signed note",
            "--chain",
            "vault",
            "--agent",
            "admin",
            "--signing-key-id",
            "a1",
            "--private-key-file",
            &admin_key,
            "--tag",
            "x",
            "--importance",
            "0.8",
            "--scope",
            "user",
        ],
    )
    .await
    .json();
    assert_eq!(signed["thought"]["content"], "signed note");

    // The friend is registered but not allowed yet.
    let friend_append = |text: &'static str, daemon: &Daemon| {
        let (rest, dashboard, key) = (
            daemon.rest.clone(),
            daemon.dashboard.clone(),
            friend_key.clone(),
        );
        async move {
            let daemon = Daemon {
                rest,
                dashboard,
                dir: PathBuf::new(),
            };
            run(
                &daemon,
                &[
                    "thought",
                    "append",
                    text,
                    "--chain",
                    "vault",
                    "--agent",
                    "friend",
                    "--signing-key-id",
                    "f1",
                    "--private-key-file",
                    &key,
                ],
            )
            .await
        }
    };
    let denied = friend_append("hi", &d).await;
    assert!(
        !denied.ok && denied.err.contains("did not accept"),
        "{}",
        denied.err
    );
    run(
        &d,
        &op(&[
            "chain",
            "access",
            "allow",
            "friend",
            "f1",
            &public_hex(2),
            "--chain",
            "vault",
        ]),
    )
    .await
    .json();
    let d = reopen_rest(&d).await;
    friend_append("hello from the friend", &d).await.json();
    run(
        &d,
        &op(&[
            "chain", "access", "deny", "friend", "f1", "--chain", "vault",
        ]),
    )
    .await
    .json();
    let d = reopen_rest(&d).await;
    assert!(!friend_append("again", &d).await.ok);
    // The operator key cannot be removed.
    let admin_view = run(&d, &op(&["chain", "access", "admin", "--chain", "vault"]))
        .await
        .json();
    let operator_id = admin_view["operator_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();
    let hijack = run(
        &d,
        &op(&[
            "chain",
            "access",
            "deny",
            "mentisdb-operator",
            &operator_id,
            "--chain",
            "vault",
        ]),
    )
    .await;
    assert!(
        !hijack.ok && hijack.err.contains("recovery"),
        "{}",
        hijack.err
    );
    // Operator commands without the PIN do nothing.
    let no_pin = run(
        &d,
        &["chain", "access", "set", "public", "--chain", "vault"],
    )
    .await;
    assert!(!no_pin.ok);

    // Every signature in the chain re-verifies (verification covers the private
    // thoughts, so it needs a key that may read them).
    let report = run(
        &d,
        &[
            "chain",
            "access",
            "verify",
            "--chain",
            "vault",
            "--read-key-file",
            &admin_key,
            "--read-agent",
            "admin",
            "--read-key-id",
            "a1",
        ],
    )
    .await
    .json();
    assert_eq!(report["invalid"], 0, "{report}");
    assert_eq!(report["policy"]["seq"], 3);
    assert!(report["valid"].as_u64().unwrap() >= 6, "{report}");
    // A public reader sees only the public opening thought.
    let public_report = run(&d, &["chain", "access", "verify", "--chain", "vault"])
        .await
        .json();
    let checks = public_report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "{public_report}");
    assert_eq!(checks[0]["status"], "unsigned");
}

/// Signing flags are validated before anything is sent.
#[tokio::test(flavor = "multi_thread")]
async fn test_signing_commands_check_their_inputs() {
    let d = start(None).await;
    let key = write_seed("03");
    let no_chain = run(
        &d,
        &[
            "thought",
            "append",
            "x",
            "--agent",
            "a",
            "--signing-key-id",
            "k",
            "--private-key-file",
            &key,
        ],
    )
    .await;
    assert!(
        !no_chain.ok && no_chain.err.contains("--chain"),
        "{}",
        no_chain.err
    );
    let no_key_id = run(
        &d,
        &[
            "thought",
            "append",
            "x",
            "--chain",
            "c",
            "--agent",
            "a",
            "--private-key-file",
            &key,
        ],
    )
    .await;
    assert!(
        !no_key_id.ok && no_key_id.err.contains("--signing-key-id"),
        "{}",
        no_key_id.err
    );
}
