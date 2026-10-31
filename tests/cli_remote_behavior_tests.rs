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
pub use mentisdb::auth;
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
        mentisdb_dir: dir,
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    };
    let dashboard = serve(dashboard_impl::dashboard_router(state)).await;
    Daemon { rest, dashboard }
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
    let d = start(None).await;
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
    let key_file = unique_dir().with_extension("key");
    std::fs::write(&key_file, format!("{}\n", "11".repeat(32))).unwrap();
    let key_file = key_file.to_str().unwrap().to_string();
    let with_key = run(
        &d,
        &[
            "agent",
            "key",
            "add",
            "bot",
            "--key-id",
            "k1",
            "--private-key-file",
            &key_file,
            "--chain",
            "reg",
        ],
    )
    .await
    .json();
    assert_eq!(with_key["agent"]["public_keys"][0]["key_id"], "k1");
    let revoked = run(
        &d,
        &[
            "agent", "key", "revoke", "bot", "--key-id", "k1", "--chain", "reg",
        ],
    )
    .await
    .json();
    assert!(revoked["agent"]["public_keys"][0]["revoked_at"].is_string());
    let disabled = run(&d, &["agent", "disable", "bot", "--chain", "reg"])
        .await
        .json();
    assert_eq!(disabled["agent"]["status"], "Revoked");
    let active = run(&d, &["agent", "activate", "bot", "--chain", "reg"])
        .await
        .json();
    assert_eq!(active["agent"]["status"], "Active");
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
