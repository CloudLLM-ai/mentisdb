//! Context-window cost of MentisDB's MCP tools on a realistic chain.
//!
//! Builds a synthetic chain of about 3,500 thoughts (realistic content
//! lengths, tags, concepts, several agents, lineages of superseded versions
//! and weak-link corrections) plus a small second chain, serves them through
//! the in-process streamable-HTTP MCP router, calls the read tools with
//! JSON-RPC `tools/call`, and prints how many characters each tool result puts
//! in front of the agent: the size of `result.content[0].text`, which is what
//! a client feeds the model, and of the whole response body, which also holds
//! `structuredContent` when the server sends it.
//!
//! Run it before and after a change to quote real numbers:
//!
//! ```sh
//! cargo run --release --example token_diet
//! ```
//!
//! The generator is deterministic (fixed seed), uses a temporary directory and
//! binds no port.

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use mentisdb::server::{standard_mcp_router, MentisDbServiceConfig};
use mentisdb::{
    MentisDb, StorageAdapterKind, Thought, ThoughtInput, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use serde_json::{json, Value};
use tower::util::ServiceExt;

const MAIN_CHAIN: &str = "project";
const SIDE_CHAIN: &str = "side";
const THOUGHTS: usize = 3_500;
const SIDE_THOUGHTS: usize = 300;
/// The topic every measured query asks about.
const QUERY: &str = "storage compaction policy";

const AREAS: [&str; 14] = [
    "storage",
    "network",
    "scheduler",
    "indexer",
    "billing",
    "auth",
    "cache",
    "installer",
    "telemetry",
    "search",
    "backup",
    "dashboard",
    "webhook",
    "migration",
];
const SUBJECTS: [&str; 10] = [
    "compaction",
    "retention",
    "throttling",
    "rollout",
    "checksum",
    "failover",
    "sharding",
    "replay",
    "pruning",
    "handshake",
];
const KINDS: [&str; 6] = [
    "policy",
    "incident",
    "benchmark",
    "constraint",
    "checklist",
    "regression",
];
const FILLER: [&str; 12] = [
    "The change was reviewed against the previous baseline before sign-off.",
    "Two outliers traced to a miscalibrated runner were excluded from the totals.",
    "Owners were notified through the shared runbook and acknowledged the plan.",
    "A follow-up is due once the next quarter's figures are reconciled.",
    "The decision stands unless the variance exceeds the agreed tolerance band.",
    "Rollback is a single configuration flip and was rehearsed on staging.",
    "Latency at the 99th percentile stayed flat while throughput doubled.",
    "The earlier assumption about disk headroom no longer holds on the new fleet.",
    "Metrics were exported to the dashboard and alerting thresholds adjusted.",
    "A note was added to the onboarding guide so new agents find this quickly.",
    "The failure reproduced only under sustained write load with cold caches.",
    "Customers were not affected; the fix shipped behind a gradual rollout.",
];
const AGENTS: [(&str, &str); 4] = [
    ("codex", "Codex"),
    ("claude", "Claude"),
    ("gemini", "Gemini"),
    ("planner", "Planner"),
];

/// Small deterministic generator, so every run measures the same chain.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

fn relation(kind: ThoughtRelationKind, target: &Thought) -> ThoughtRelation {
    ThoughtRelation {
        kind,
        target_id: target.id,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }
}

/// Realistic body text: a headline naming area, subject and kind, then two to
/// six filler sentences.
fn body(rng: &mut Lcg, area: &str, subject: &str, kind: &str, revision: usize) -> String {
    let mut text = format!("{area} {subject} {kind}, revision {revision}: ");
    for _ in 0..(2 + rng.next(5)) {
        text.push_str(FILLER[rng.next(FILLER.len())]);
        text.push(' ');
    }
    text.trim_end().to_owned()
}

fn thought_type(rng: &mut Lcg) -> (ThoughtType, ThoughtRole, f32) {
    match rng.next(10) {
        0 => (ThoughtType::Decision, ThoughtRole::Memory, 0.8),
        1 => (ThoughtType::Constraint, ThoughtRole::Memory, 0.9),
        2 => (ThoughtType::LessonLearned, ThoughtRole::Memory, 0.7),
        3 | 4 => (ThoughtType::Insight, ThoughtRole::Memory, 0.5),
        5 => (ThoughtType::Finding, ThoughtRole::Memory, 0.6),
        6 => (ThoughtType::PreferenceUpdate, ThoughtRole::Memory, 0.8),
        _ => (ThoughtType::Finding, ThoughtRole::Memory, 0.4),
    }
}

fn decorate(rng: &mut Lcg, input: ThoughtInput, area: &str, subject: &str) -> ThoughtInput {
    let mut input = input
        .with_tags(vec![area.to_owned(), subject.to_owned()])
        .with_concepts(vec![
            format!("{area} {subject}"),
            format!("{subject} review"),
        ]);
    if rng.next(2) == 0 {
        input = input.with_confidence(0.6 + 0.05 * rng.next(8) as f32);
    }
    input
}

/// Seeds [`MAIN_CHAIN`]: mostly plain thoughts, about 70 lineages of two to
/// thirty superseded versions (one of them on [`QUERY`]'s topic) and about 50
/// weak-link corrections that only reference the thought they replace.
fn seed_main(dir: &std::path::Path) -> std::io::Result<()> {
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, MAIN_CHAIN, StorageAdapterKind::Binary)?;
    let mut rng = Lcg(0x5eed_cafe);
    let mut appended = 0usize;
    let mut lineages = 0usize;
    let mut corrections = 0usize;
    // The query topic carries a long lineage, as in a real, long-lived chain.
    let mut plan: Vec<(usize, &str, &str)> = vec![(30, "storage", "compaction")];
    while appended < THOUGHTS {
        let area = AREAS[rng.next(AREAS.len())];
        let subject = SUBJECTS[rng.next(SUBJECTS.len())];
        let (agent, _) = AGENTS[rng.next(AGENTS.len())];
        let kind = KINDS[rng.next(KINDS.len())];
        if let Some((versions, a, s)) = plan.pop() {
            let mut previous: Option<Thought> = None;
            for v in 1..=versions {
                let text = body(&mut rng, a, s, "policy", v);
                let mut input = ThoughtInput::new(ThoughtType::Decision, text).with_importance(0.8);
                input = decorate(&mut rng, input, a, s);
                if let Some(prev) = &previous {
                    input =
                        input.with_relations(vec![relation(ThoughtRelationKind::Supersedes, prev)]);
                }
                previous = Some(chain.append_thought(agent, input)?.clone());
                appended += 1;
            }
            continue;
        }
        let roll = rng.next(100);
        if roll < 2 && lineages < 70 {
            lineages += 1;
            plan.push((2 + rng.next(12), area, subject));
            continue;
        }
        let (tt, role, importance) = thought_type(&mut rng);
        let text = body(&mut rng, area, subject, kind, 1);
        let input = ThoughtInput::new(tt, text)
            .with_role(role)
            .with_importance(importance);
        let first = chain
            .append_thought(agent, decorate(&mut rng, input, area, subject))?
            .clone();
        appended += 1;
        if roll < 4 && corrections < 50 {
            corrections += 1;
            chain.append_thought(
                agent,
                ThoughtInput::new(
                    ThoughtType::Correction,
                    format!(
                        "Correction: the earlier {area} {subject} {kind} was wrong. {}",
                        FILLER[rng.next(FILLER.len())]
                    ),
                )
                .with_refs(vec![first.index])
                .with_importance(0.8),
            )?;
            appended += 1;
        }
    }
    Ok(())
}

fn seed_side(dir: &std::path::Path) -> std::io::Result<()> {
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, SIDE_CHAIN, StorageAdapterKind::Binary)?;
    let mut rng = Lcg(0xfeed);
    for _ in 0..SIDE_THOUGHTS {
        let area = AREAS[rng.next(AREAS.len())];
        let subject = SUBJECTS[rng.next(SUBJECTS.len())];
        let (tt, _, importance) = thought_type(&mut rng);
        let text = body(&mut rng, area, subject, "note", 1);
        let input = ThoughtInput::new(tt, text).with_importance(importance);
        chain.append_thought("codex", decorate(&mut rng, input, area, subject))?;
    }
    Ok(())
}

/// One JSON-RPC `tools/call`; returns `(text_chars, body_chars)`.
async fn call(router: &axum::Router, tool: &str, arguments: Value) -> (usize, usize) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": tool, "arguments": arguments}})
            .to_string(),
        ))
        .expect("request");
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49003,
        ))));
    let response = router.clone().oneshot(request).await.expect("response");
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .expect("body");
    let value: Value = serde_json::from_slice(&bytes).expect("json-rpc response");
    let text = value["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        value["result"]["isError"] != json!(true),
        "{tool} failed: {text}"
    );
    (
        text.chars().count(),
        String::from_utf8_lossy(&bytes).chars().count(),
    )
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    seed_main(dir.path())?;
    seed_side(dir.path())?;
    let config = MentisDbServiceConfig::new(
        dir.path().to_path_buf(),
        MAIN_CHAIN,
        StorageAdapterKind::Binary,
    )
    .with_bearer_token_access(false);
    let (router, _events) =
        standard_mcp_router(config, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

    let cases: Vec<(&str, &str, Value)> = vec![
        (
            "ranked_search (default)",
            "mentisdb_ranked_search",
            json!({"chain_key": MAIN_CHAIN, "text": QUERY, "limit": 10}),
        ),
        (
            "ranked_search compact:true",
            "mentisdb_ranked_search",
            json!({"chain_key": MAIN_CHAIN, "text": QUERY, "limit": 10, "compact": true}),
        ),
        (
            "ranked_search compact:false",
            "mentisdb_ranked_search",
            json!({"chain_key": MAIN_CHAIN, "text": QUERY, "limit": 10, "compact": false}),
        ),
        (
            "lexical_search (default)",
            "mentisdb_lexical_search",
            json!({"chain_key": MAIN_CHAIN, "text": QUERY, "limit": 10}),
        ),
        (
            "federated_search (default)",
            "mentisdb_federated_search",
            json!({"chain_keys": [MAIN_CHAIN, SIDE_CHAIN], "text": QUERY, "limit": 10}),
        ),
        (
            "context_bundles (default)",
            "mentisdb_context_bundles",
            json!({"chain_key": MAIN_CHAIN, "text": QUERY, "limit": 5}),
        ),
        (
            "standing_context (default)",
            "mentisdb_standing_context",
            json!({"chain_key": MAIN_CHAIN}),
        ),
        (
            "recent_context last_n=20",
            "mentisdb_recent_context",
            json!({"chain_key": MAIN_CHAIN, "last_n": 20}),
        ),
    ];
    println!(
        "chain: {THOUGHTS} thoughts + {SIDE_THOUGHTS} on a side chain, query {QUERY:?}, limit 10"
    );
    println!(
        "{:<32} {:>12} {:>14}",
        "tool call", "text chars", "response chars"
    );
    for (label, tool, arguments) in cases {
        let (text, whole) = call(&router, tool, arguments).await;
        println!("{label:<32} {text:>12} {whole:>14}");
    }
    Ok(())
}
