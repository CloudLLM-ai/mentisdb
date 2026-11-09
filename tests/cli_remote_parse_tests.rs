//! Offline tests for the daemon-facing CLI commands: grammar, typed argument
//! validation, request building and help. Nothing here touches the network;
//! behavior against a real REST server lives in `cli_remote_behavior_tests.rs`.

use mentisdb::cli::remote::{HttpMethod, RemoteInvocation, Render, Transport};
use mentisdb::cli::{help_text, parse_args, remote_commands, CliCommand};
use serde_json::{json, Value};

/// Parse `mentisdb <args...>` into a remote invocation.
fn invocation(args: &[&str]) -> RemoteInvocation {
    let argv = std::iter::once("mentisdb").chain(args.iter().copied());
    match parse_args(argv) {
        Ok(CliCommand::Remote(inv)) => *inv,
        other => panic!("expected a remote command for {args:?}, got {other:?}"),
    }
}

/// Parse and return the error message.
fn rejection(args: &[&str]) -> String {
    let argv = std::iter::once("mentisdb").chain(args.iter().copied());
    parse_args(argv).expect_err("expected a parse error")
}

/// Parse and return the help text.
fn help(args: &[&str]) -> String {
    let argv = std::iter::once("mentisdb").chain(args.iter().copied());
    match parse_args(argv) {
        Ok(CliCommand::RemoteHelp(text)) => text,
        other => panic!("expected help for {args:?}, got {other:?}"),
    }
}

fn body(args: &[&str]) -> Value {
    invocation(args).body.expect("command posts a body")
}

const UUID: &str = "7c0d3a52-1b5e-4f4a-9a31-0a8b1d2e3f40";

/// Scalar, list and nested flags land on the REST field names.
#[test]
fn test_thought_append_maps_flags_to_rest_fields() {
    let inv = invocation(&[
        "thought",
        "append",
        "Use UTC",
        "--type",
        "decision",
        "--role",
        "working-memory",
        "--importance",
        "0.9",
        "--tag",
        "a",
        "--tag",
        "b",
        "--ref",
        "3",
        "--chain",
        "p",
        "--relation",
        &format!("corrects:{UUID}@other"),
    ]);
    assert_eq!(inv.method, HttpMethod::Post);
    assert_eq!(inv.transport, Transport::Rest);
    assert_eq!(inv.url, "http://127.0.0.1:9472/v1/thoughts");
    let body = inv.body.unwrap();
    assert_eq!(body["content"], "Use UTC");
    assert_eq!(body["thought_type"], "Decision");
    assert_eq!(body["role"], "WorkingMemory");
    assert_eq!(body["importance"], 0.9);
    assert_eq!(body["tags"], json!(["a", "b"]));
    assert_eq!(body["refs"], json!([3]));
    assert_eq!(body["chain_key"], "p");
    assert_eq!(
        body["relations"],
        json!([{"kind": "corrects", "target_id": UUID, "chain_key": "other"}])
    );
}

/// `--type` defaults to FactLearned because the REST API requires one.
#[test]
fn test_thought_append_defaults_type() {
    assert_eq!(
        body(&["thought", "append", "x"])["thought_type"],
        "FactLearned"
    );
}

/// `-` as content defers to standard input instead of sending a literal dash.
#[test]
fn test_dash_content_reads_stdin() {
    let inv = invocation(&["thought", "append", "-"]);
    assert_eq!(inv.stdin_field.as_deref(), Some("content"));
    assert!(inv.body.unwrap().get("content").is_none());
}

/// Typed values are rejected before any request is built.
#[test]
fn test_invalid_values_are_rejected_locally() {
    assert!(
        rejection(&["thought", "append", "x", "--type", "nope"]).contains("Unknown ThoughtType")
    );
    assert!(
        rejection(&["thought", "append", "x", "--importance", "1.5"])
            .contains("between 0.0 and 1.0")
    );
    assert!(rejection(&["thought", "append", "x", "--role", "boss"]).contains("is not one of"));
    assert!(rejection(&["thought", "get", "--id", "not-a-uuid"]).contains("expected a UUID"));
    assert!(rejection(&["search", "q", "--since", "yesterday"]).contains("RFC 3339"));
    assert!(rejection(&["search", "q", "--limit", "many"]).contains("expected an integer"));
    // Operator key arguments are validated by the operator code, not the parser;
    // a missing one is still caught here.
    assert!(rejection(&["chain", "access", "allow", "a", "k", "--chain", "c"]).contains("missing"));
}

/// Unknown flags, missing required arguments and extra positionals name the command.
#[test]
fn test_usage_errors_name_the_command() {
    assert!(rejection(&["thought", "append", "x", "--bogus"]).contains("Unknown flag '--bogus'"));
    assert!(rejection(&["thought", "append"]).contains("missing <content>"));
    assert!(rejection(&["chain", "branch", "--from", "a"]).contains("--at"));
    assert!(rejection(&["thought", "genesis", "extra"]).contains("Unexpected argument 'extra'"));
    assert!(rejection(&["thought", "append", "x", "--tag"]).contains("requires a value"));
    assert!(rejection(&["thought", "bogus"]).contains("Unknown subcommand 'bogus'"));
}

/// Destructive commands refuse to run without `--yes`.
#[test]
fn test_destructive_commands_require_yes() {
    for args in [
        vec!["chain", "delete", "c"],
        vec!["chain", "merge", "--from", "a", "--into", "b"],
        vec!["skill", "delete", "s"],
        vec!["vectors", "rebuild"],
        vec!["daemon", "restart"],
    ] {
        assert!(
            rejection(&args).contains("--yes"),
            "{args:?} must need --yes"
        );
    }
    assert!(invocation(&["chain", "delete", "c", "--yes"])
        .body
        .is_none());
}

/// `--flag=value` and short aliases behave like the spaced long form.
#[test]
fn test_inline_values_and_short_flags() {
    assert_eq!(
        body(&["thought", "get", "--index=4", "-c", "k"])["thought_index"],
        4
    );
    assert_eq!(body(&["thought", "get", "-c", "k"])["chain_key"], "k");
}

/// A query equal to a mode name is reachable after `--`.
#[test]
fn test_double_dash_allows_mode_named_queries() {
    assert_eq!(body(&["search", "--", "lexical"])["text"], "lexical");
    assert_eq!(
        invocation(&["search", "lexical", "x"]).url,
        "http://127.0.0.1:9472/v1/lexical-search"
    );
}

/// `--compact` is a switch on `search` and `search federated` that lands on the
/// REST/MCP `compact` field; it is absent unless given, and `search bundles`
/// (where the field has no effect) does not offer it.
#[test]
fn test_compact_flag_on_ranked_and_federated_search() {
    assert!(body(&["search", "q"]).get("compact").is_none());
    assert_eq!(body(&["search", "q", "--compact"])["compact"], true);
    let federated = body(&["search", "federated", "q", "-c", "a", "--compact"]);
    assert_eq!(federated["compact"], true);
    assert_eq!(federated["chain_keys"], json!(["a"]));
    let argv = ["mentisdb", "search", "bundles", "q", "--compact"];
    assert!(parse_args(argv.iter().copied()).is_err());
}

/// Dotted fields nest, and `--include-anchor` takes an explicit boolean.
#[test]
fn test_nested_and_boolean_fields() {
    let b = body(&[
        "search",
        "q",
        "--graph-depth",
        "2",
        "--graph-mode",
        "bidirectional",
        "--rerank",
    ]);
    assert_eq!(b["graph"], json!({"max_depth": 2, "mode": "bidirectional"}));
    assert_eq!(b["enable_reranking"], true);
    let t = body(&[
        "thought",
        "traverse",
        "--from",
        "head",
        "--include-anchor",
        "false",
        "--window-start",
        "5",
        "--window-delta",
        "9",
        "--window-unit",
        "seconds",
    ]);
    assert_eq!(t["anchor_boundary"], "head");
    assert_eq!(t["include_anchor"], false);
    assert_eq!(
        t["time_window"],
        json!({"start": 5, "delta": 9, "unit": "seconds"})
    );
}

/// Timestamps are normalized to UTC.
#[test]
fn test_timestamps_normalize_to_utc() {
    let b = body(&["search", "q", "--since", "2026-01-01T02:00:00+02:00"]);
    assert_eq!(b["since"], "2026-01-01T00:00:00+00:00");
}

/// Key registration is an operator command: it goes to the dashboard route as
/// one `register_key` operation, with the chain in the path and no proof of
/// possession (the operator vouches for the key).
#[test]
fn test_agent_key_add_is_an_operator_operation() {
    let hex = "ab".repeat(32);
    let inv = invocation(&[
        "agent",
        "key",
        "add",
        "bot",
        "--key-id",
        "k1",
        "--public-key-hex",
        &hex,
        "--chain",
        "vault",
    ]);
    assert_eq!(
        inv.url,
        "https://127.0.0.1:9475/dashboard/api/chains/vault/access"
    );
    let b = inv.body.unwrap();
    assert_eq!(b["op"], "register_key");
    assert_eq!(b["agent_id"], "bot");
    assert_eq!(b["key_id"], "k1");
    assert_eq!(b["public_key"], hex.as_str());
    // The key, the chain and the key id are all required.
    for missing in [
        &[
            "agent", "key", "add", "bot", "--key-id", "k1", "--chain", "c",
        ][..],
        &[
            "agent",
            "key",
            "add",
            "bot",
            "--public-key-hex",
            &hex,
            "--chain",
            "c",
        ][..],
        &[
            "agent",
            "key",
            "add",
            "bot",
            "--key-id",
            "k1",
            "--public-key-hex",
            &hex,
        ][..],
    ] {
        let error = rejection(missing);
        assert!(error.contains("missing"), "{error}");
    }
}

/// `agent key generate` is an operator command shaped like `agent key add`
/// (same route, same `register_key` operation, same connection flags) that
/// makes the public key itself: the body has no `public_key` yet, `--out` and
/// `--force` stay inside the CLI, and `--key-id` and `--chain` are required.
#[test]
fn test_agent_key_generate_is_an_operator_command_that_creates_the_key() {
    let inv = invocation(&[
        "agent",
        "key",
        "generate",
        "builder",
        "--key-id",
        "builder-2026",
        "--chain",
        "project",
        "--out",
        "/tmp/k.key",
        "--force",
        "--pin",
        "123456",
    ]);
    assert!(inv.generates_key);
    assert_eq!(inv.transport, Transport::Dashboard);
    assert_eq!(
        inv.url,
        "https://127.0.0.1:9475/dashboard/api/chains/project/access"
    );
    assert_eq!(inv.credential.as_deref(), Some("123456"));
    let b = inv.body.unwrap();
    assert_eq!(b["op"], "register_key");
    assert_eq!(b["agent_id"], "builder");
    assert_eq!(b["key_id"], "builder-2026");
    assert!(b.get("public_key").is_none(), "{b}");
    assert!(b.get("out").is_none() && b.get("force").is_none(), "{b}");
    assert_eq!(inv.local["out"], "/tmp/k.key");
    assert_eq!(inv.local["force"], true);
    assert!(
        invocation(&[
            "agent",
            "key",
            "generate",
            "b",
            "--key-id",
            "k",
            "--chain",
            "c",
            "--offline"
        ])
        .offline
    );
    assert!(
        !invocation(&[
            "agent",
            "key",
            "add",
            "b",
            "--key-id",
            "k",
            "--public-key-hex",
            &"ab".repeat(32),
            "--chain",
            "c"
        ])
        .generates_key
    );
    for missing in [
        &["agent", "key", "generate", "b", "--chain", "c"][..],
        &["agent", "key", "generate", "b", "--key-id", "k"][..],
        &["agent", "key", "generate", "--key-id", "k", "--chain", "c"][..],
    ] {
        assert!(rejection(missing).contains("missing"), "{missing:?}");
    }
    let unknown = rejection(&[
        "agent",
        "key",
        "generate",
        "b",
        "--key-id",
        "k",
        "--chain",
        "c",
        "--public-key-hex",
        "ab",
    ]);
    assert!(unknown.contains("Unknown flag"), "{unknown}");
}

/// The help of the new command, and the helps that point at it, say what it
/// does with the private key.
#[test]
fn test_agent_key_generate_help_warns_about_the_private_key() {
    let text = help(&["agent", "key", "generate", "--help"]);
    for needle in [
        "mentisdb agent key generate <agent-id>",
        "--key-id",
        "--out",
        "--force",
        "--pin",
        "--offline",
        "NEVER printed",
        "only copy",
        "never paste it into a chat",
        "agent key add",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
    assert!(help(&["agent", "key", "add", "--help"]).contains("mentisdb agent key generate"));
    assert!(help(&["agent", "--help"]).contains("agent key generate"));
    assert!(mentisdb::cli::key::KEY_HELP.contains("mentisdb agent key generate"));
}

/// Operator commands are offline-capable and PIN-gated, never REST-token commands.
#[test]
fn test_operator_commands_target_the_dashboard() {
    let hex = "cd".repeat(32);
    for (args, op) in [
        (
            vec!["chain", "access", "set", "private", "--chain", "v"],
            "set_mode",
        ),
        (
            vec!["chain", "access", "allow", "a", "k", &hex, "--chain", "v"],
            "allow_key",
        ),
        (
            vec!["chain", "access", "deny", "a", "k", "--chain", "v"],
            "deny_key",
        ),
        (
            vec![
                "agent", "key", "revoke", "a", "--key-id", "k", "--chain", "v",
            ],
            "revoke_key",
        ),
        (
            vec!["agent", "disable", "a", "--chain", "v"],
            "disable_agent",
        ),
        (
            vec!["agent", "activate", "a", "--chain", "v"],
            "enable_agent",
        ),
    ] {
        let inv = invocation(&args);
        assert!(
            inv.url.contains("/dashboard/api/chains/v/access"),
            "{args:?}: {}",
            inv.url
        );
        assert_eq!(inv.body.unwrap()["op"], op, "{args:?}");
    }
    let inv = invocation(&[
        "chain",
        "access",
        "set",
        "public",
        "--chain",
        "v",
        "--offline",
        "--data-dir",
        "/x",
    ]);
    assert!(inv.offline);
    assert_eq!(inv.data_dir.as_deref(), Some("/x"));
    // `public` and `private` are accepted names for the modes.
    assert_eq!(
        body(&["chain", "access", "set", "public", "--chain", "v"])["mode"],
        "public"
    );
    assert!(
        rejection(&["chain", "access", "set", "sideways", "--chain", "v"])
            .contains("is not one of")
    );
}

/// A read key needs its agent and key id; the seed is redacted from Debug.
#[test]
fn test_read_key_flags() {
    let dir = std::env::temp_dir().join(format!("mentisdb_cli_readkey_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("r.key");
    std::fs::write(&path, format!("{}\n", "09".repeat(32))).unwrap();
    let path = path.to_str().unwrap();
    let inv = invocation(&[
        "thought",
        "get",
        "--index",
        "0",
        "--read-key-file",
        path,
        "--read-agent",
        "reader",
        "--read-key-id",
        "r1",
    ]);
    let key = inv.read_key.as_ref().expect("read key");
    assert_eq!(
        (key.agent_id.as_str(), key.key_id.as_str()),
        ("reader", "r1")
    );
    assert!(!format!("{inv:?}").contains("0909"));
    let error = rejection(&["thought", "get", "--index", "0", "--read-key-file", path]);
    assert!(error.contains("--read-agent"), "{error}");
    std::fs::write(dir.join("short.key"), "abcd").unwrap();
    let error = rejection(&[
        "thought",
        "get",
        "--index",
        "0",
        "--read-key-file",
        dir.join("short.key").to_str().unwrap(),
        "--read-agent",
        "a",
        "--read-key-id",
        "k",
    ]);
    assert!(error.contains("32 bytes"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `agents` is the historical spelling of `agent list` and now POSTs, as the route requires.
#[test]
fn test_agents_alias_posts_to_agents_route() {
    let inv = invocation(&["agents", "--chain", "c"]);
    assert_eq!(inv.method, HttpMethod::Post);
    assert_eq!(inv.url, "http://127.0.0.1:9472/v1/agents");
    assert_eq!(inv.body.unwrap()["chain_key"], "c");
}

/// Federated search takes a repeatable chain flag and requires at least one.
#[test]
fn test_federated_search_collects_chains() {
    let b = body(&["search", "federated", "q", "--chain", "a", "--chain", "b"]);
    assert_eq!(b["chain_keys"], json!(["a", "b"]));
    assert!(rejection(&["search", "federated", "q"]).contains("--chain"));
}

/// Path parameters are percent-encoded; GET and DELETE carry no body.
#[test]
fn test_dashboard_commands_build_paths_and_headers() {
    let inv = invocation(&["chain", "info", "my chain/1"]);
    assert_eq!(inv.transport, Transport::Dashboard);
    assert_eq!(inv.method, HttpMethod::Get);
    assert_eq!(
        inv.url,
        "https://127.0.0.1:9475/dashboard/api/chains/my%20chain%2F1"
    );
    assert!(inv.body.is_none());
    let del = invocation(&[
        "chain",
        "delete",
        "c",
        "--yes",
        "--dashboard-url",
        "https://h:1/",
    ]);
    assert_eq!(del.method, HttpMethod::Delete);
    assert_eq!(del.url, "https://h:1/dashboard/api/chains/c");
    let copy = invocation(&["agent", "copy", "bot", "--from", "a", "--to", "b"]);
    assert_eq!(
        copy.url,
        "https://127.0.0.1:9475/dashboard/api/agents/a/bot/copy-to/b"
    );
}

/// Query parameters go on the URL and the diff prints the `diff` field raw.
#[test]
fn test_skill_diff_uses_query_string_and_text_render() {
    let inv = invocation(&["skill", "diff", "s", "--from", UUID]);
    assert_eq!(
        inv.url,
        format!("https://127.0.0.1:9475/dashboard/api/skills/s/diff?from={UUID}")
    );
    assert_eq!(inv.render, Render::Text("diff"));
}

/// `NAME=VALUE` pairs collect into the `settings` object.
#[test]
fn test_settings_set_collects_pairs() {
    let b = body(&[
        "daemon",
        "settings",
        "set",
        "MENTISDB_AUTO_FLUSH=false",
        "MENTISDB_VERBOSE=true",
    ]);
    assert_eq!(
        b["settings"],
        json!({"MENTISDB_AUTO_FLUSH": "false", "MENTISDB_VERBOSE": "true"})
    );
    assert!(rejection(&["daemon", "settings", "set", "oops"]).contains("NAME=VALUE"));
}

/// Webhook ids travel in the path of a DELETE.
#[test]
fn test_webhook_delete_uses_path_id() {
    let inv = invocation(&["webhook", "delete", UUID]);
    assert_eq!(inv.method, HttpMethod::Delete);
    assert_eq!(inv.url, format!("http://127.0.0.1:9472/v1/webhooks/{UUID}"));
}

/// Connection flags apply to every command and never leak into the body.
#[test]
fn test_connection_flags() {
    let inv = invocation(&[
        "chain",
        "list",
        "--url",
        "http://h:9/",
        "--json",
        "--token",
        "t",
        "--insecure",
    ]);
    assert_eq!(inv.url, "http://h:9/v1/chains");
    assert!(inv.json && inv.insecure);
    assert_eq!(inv.credential.as_deref(), Some("t"));
    let b = body(&["thought", "genesis", "--url", "http://h:9", "--json"]);
    assert!(b.get("url").is_none() && b.get("json").is_none());
}

/// `--help`, `help <cmd>` and bare group names print help instead of running.
#[test]
fn test_help_forms() {
    let direct = help(&["chain", "branch", "--help"]);
    assert!(
        direct.contains("Usage:")
            && direct.contains("--from <KEY>")
            && direct.contains("Examples:")
    );
    assert_eq!(help(&["help", "chain", "branch"]), direct);
    assert!(help(&["chain"]).contains("chain branch"));
    assert!(help(&["search", "--help"]).contains("Ranked search"));
}

/// Existing local commands still parse after the catalog was added.
#[test]
fn test_local_commands_are_unaffected() {
    assert!(matches!(
        parse_args(["mentisdb", "backup"]),
        Ok(CliCommand::Backup(_))
    ));
    assert!(matches!(
        parse_args(["mentisdb", "add", "x"]),
        Ok(CliCommand::Add(_))
    ));
    assert!(matches!(
        parse_args(["mentisdb", "bearertoken", "list"]),
        Ok(CliCommand::BearerToken(_))
    ));
    assert!(parse_args(["mentisdb", "nonsense"]).is_err());
}

/// Every catalogued command is documented, exampled, listed in the overview and in the parity map.
#[test]
fn test_every_command_is_documented() {
    let parity =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/CLI_PARITY.md"))
            .expect("docs/CLI_PARITY.md exists");
    let overview = help_text();
    for command in remote_commands() {
        let path = command.path.join(" ");
        assert!(
            !command.summary.is_empty() && !command.about.is_empty(),
            "{path}: no description"
        );
        assert!(!command.examples.is_empty(), "{path}: no examples");
        assert!(
            command.params.iter().all(|f| !f.help.is_empty()),
            "{path}: flag without help"
        );
        assert!(
            command
                .examples
                .iter()
                .all(|e| e.starts_with(&format!("mentisdb {path}"))
                    || e.contains(&format!("mentisdb {path}"))),
            "{path}: example does not run this command"
        );
        assert!(
            overview.contains(&path),
            "{path}: missing from mentisdb --help"
        );
        assert!(
            parity.contains(&format!("`{path}")),
            "{path}: missing from docs/CLI_PARITY.md"
        );
        let text = help(
            &command
                .path
                .iter()
                .copied()
                .chain(["--help"])
                .collect::<Vec<_>>(),
        );
        assert!(
            text.contains("Usage:") && text.contains("Examples:"),
            "{path}: incomplete help"
        );
    }
}

/// Every flag name is unique within its command so parsing is unambiguous.
#[test]
fn test_flag_names_are_unique_per_command() {
    for command in remote_commands() {
        let mut names: Vec<&str> = command.params.iter().map(|f| f.long).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "{}: duplicate flag",
            command.path.join(" ")
        );
    }
}

/// Regression (found by the `cli_args` fuzz target): `search q --url thought`
/// was accepted and produced the request URL `thought/v1/ranked-search`. A base
/// URL must be an absolute http(s) URL, and an invalid one is refused before any
/// request is built.
#[test]
fn test_connection_urls_must_be_absolute_http_urls() {
    for bad in ["thought", "127.0.0.1:9472", "ftp://h/", "http://", ""] {
        let error = rejection(&["search", "q", "--url", bad]);
        assert!(
            error.contains("--url") && error.contains("http"),
            "{bad}: {error}"
        );
    }
    let error = rejection(&["chain", "info", "c", "--dashboard-url", "nope"]);
    assert!(error.contains("--dashboard-url"), "{error}");
    assert_eq!(
        invocation(&["chain", "list", "--url", "https://h:1/base/"]).url,
        "https://h:1/base/v1/chains"
    );
}
