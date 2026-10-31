//! Payload-shape regression tests for the `mentisdb add` and `mentisdb search`
//! CLI subcommands.
//!
//! GitHub issue #17 reported that these subcommands produced REST payloads the
//! daemon rejected because the required `thought_type` field was missing. These
//! tests pin the outgoing JSON body so the CLI and the REST schema cannot drift
//! again without a failing test.

use mentisdb::cli::{build_add_body, parse_args, AddCommand, CliCommand};

fn add_command(content: &str, thought_type: Option<&str>) -> AddCommand {
    AddCommand {
        content: content.to_string(),
        thought_type: thought_type.map(str::to_string),
        scope: None,
        tags: Vec::new(),
        agent_id: None,
        chain_key: None,
        url: "http://127.0.0.1:9472".to_string(),
    }
}

/// Parse `mentisdb search ...` and return the JSON body it would POST.
fn search_body(args: &[&str]) -> serde_json::Value {
    let argv = std::iter::once("mentisdb").chain(args.iter().copied());
    match parse_args(argv).expect("search should parse") {
        CliCommand::Remote(invocation) => invocation.body.expect("search posts a body"),
        other => panic!("expected a remote command, got {other:?}"),
    }
}

/// The REST API requires `thought_type`, so omitting `--type` must still emit
/// the documented default rather than an empty body field.
#[test]
fn add_body_defaults_thought_type_to_fact_learned() {
    let body = build_add_body(&add_command("smoke test", None)).unwrap();
    assert_eq!(body["thought_type"], "FactLearned");
    assert_eq!(body["content"], "smoke test");
}

/// Explicit types are normalized to their canonical PascalCase name.
#[test]
fn add_body_normalizes_explicit_thought_type() {
    assert_eq!(
        build_add_body(&add_command("x", Some("insight"))).unwrap()["thought_type"],
        "Insight"
    );
    assert_eq!(
        build_add_body(&add_command("x", Some("fact-learned"))).unwrap()["thought_type"],
        "FactLearned"
    );
    // `Goal` was missing from the server's old hand-written parser; the unified
    // parser accepts it and the CLI forwards it.
    assert_eq!(
        build_add_body(&add_command("x", Some("goal"))).unwrap()["thought_type"],
        "Goal"
    );
}

/// A typo in `--type` fails locally with an actionable message instead of an
/// opaque server 4xx.
#[test]
fn add_body_rejects_unknown_thought_type_with_valid_options() {
    let error = build_add_body(&add_command("x", Some("note"))).unwrap_err();
    assert!(
        error.contains("Unknown ThoughtType 'note'"),
        "unexpected error: {error}"
    );
    assert!(
        error.contains("FactLearned"),
        "error should list valid types"
    );
    assert!(error.contains("Insight"), "error should list valid types");
}

/// Optional flags map onto their REST field names.
#[test]
fn add_body_carries_optional_fields() {
    let mut cmd = add_command("x", Some("decision"));
    cmd.scope = Some("session".to_string());
    cmd.tags = vec!["a".to_string(), "b".to_string()];
    cmd.agent_id = Some("agent-1".to_string());
    cmd.chain_key = Some("chain-1".to_string());

    let body = build_add_body(&cmd).unwrap();
    assert_eq!(body["scope"], "session");
    assert_eq!(body["tags"], serde_json::json!(["a", "b"]));
    assert_eq!(body["agent_id"], "agent-1");
    assert_eq!(body["chain_key"], "chain-1");
}

/// Ranked search reads the query from `text`, not `query`.
#[test]
fn search_body_uses_text_field() {
    let body = search_body(&[
        "search",
        "cache invalidation",
        "--limit",
        "5",
        "--scope",
        "user",
        "--chain",
        "chain-1",
    ]);
    assert_eq!(body["text"], "cache invalidation");
    assert_eq!(body["limit"], 5);
    assert_eq!(body["scope"], "user");
    assert_eq!(body["chain_key"], "chain-1");
    assert!(
        body.get("query").is_none(),
        "must not send the old query field"
    );
}

/// `mentisdb thought standing` posts to the standing-context route with typed,
/// canonicalized filters and prints the `prompt` field.
#[test]
fn thought_standing_body_carries_filters() {
    let argv = [
        "mentisdb",
        "thought",
        "standing",
        "--chain",
        "p",
        "--type",
        "decision",
        "--type",
        "constraint",
        "--per-type-limit",
        "3",
        "--limit",
        "10",
        "--include-invalidated",
    ];
    match parse_args(argv).expect("thought standing should parse") {
        CliCommand::Remote(invocation) => {
            assert!(
                invocation.url.ends_with("/v1/standing-context"),
                "{}",
                invocation.url
            );
            let body = invocation.body.expect("posts a body");
            assert_eq!(body["chain_key"], "p");
            assert_eq!(body["types"], serde_json::json!(["Decision", "Constraint"]));
            assert_eq!(body["per_type_limit"], 3);
            assert_eq!(body["limit"], 10);
            assert_eq!(body["include_invalidated"], true);
        }
        other => panic!("expected a remote command, got {other:?}"),
    }
}
