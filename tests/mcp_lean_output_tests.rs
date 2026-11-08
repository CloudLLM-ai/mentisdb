//! The streamable-HTTP MCP surface returns lean results; REST and
//! `/tools/execute` keep their documented shapes.
//!
//! Covers [`mentisdb::server_lean`]: minified text with no `structuredContent`,
//! lean search hits by default with `compact: false` as the opt-out, what a
//! lean hit must still carry (version status, ids, content), and standing
//! context returned once.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use mentisdb::server::{mcp_router, standard_mcp_router, MentisDbServiceConfig};
use mentisdb::{
    MentisDb, StorageAdapterKind, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Value};
use tower::util::ServiceExt;

const CHAIN: &str = "lean";

/// A chain with filler (keeps query terms under the document-frequency
/// cutoff), a lineage of three versions, a weak-link correction and a
/// `Constraint` for standing context. Returns the directory and the ids of the
/// versions, oldest first.
fn seed() -> (tempfile::TempDir, Vec<String>) {
    // Lexical only: a chain this size starts a background embedding-model load,
    // and a test process that exits while it runs crashes inside onnxruntime.
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    for i in 0..300 {
        chain
            .append_thought(
                "filler",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!("Gardening entry {i}: tomatoes."),
                ),
            )
            .unwrap();
    }
    let mut ids = Vec::new();
    let mut previous = None;
    for v in 1..=3 {
        let mut input = ThoughtInput::new(
            ThoughtType::Decision,
            format!("Nightly build server policy revision {v}: keep the runner warm."),
        );
        if let Some(prev) = &previous {
            input = input.with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::Supersedes,
                target_id: *prev,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]);
        }
        let t = chain.append_thought("agent", input).unwrap().clone();
        previous = Some(t.id);
        ids.push(t.id.to_string());
    }
    let old = chain
        .append_thought(
            "agent",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Weekly report server schedule: Mondays.",
            ),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(
                ThoughtType::Correction,
                "Correction: the earlier report schedule was wrong, send on Tuesdays.",
            )
            .with_refs(vec![old.index]),
        )
        .unwrap();
    chain
        .append_thought(
            "agent",
            ThoughtInput::new(
                ThoughtType::Constraint,
                "Never deploy the build server on Fridays.",
            ),
        )
        .unwrap();
    (dir, ids)
}

fn config(dir: &tempfile::TempDir) -> MentisDbServiceConfig {
    MentisDbServiceConfig::new(dir.path().to_path_buf(), CHAIN, StorageAdapterKind::Binary)
        .with_bearer_token_access(false)
}

fn with_peer(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49003,
        ))));
    request
}

/// JSON-RPC `tools/call` over the streamable MCP router; returns the whole
/// `result` object.
async fn rpc(dir: &tempfile::TempDir, tool: &str, arguments: Value) -> Value {
    let (router, _events) = standard_mcp_router(
        config(dir),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    );
    let request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": tool, "arguments": arguments}})
            .to_string(),
        ))
        .unwrap();
    let response = router.oneshot(with_peer(request)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_ne!(value["result"]["isError"], json!(true), "{tool}: {value}");
    value["result"].clone()
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().expect("text content")
}

fn parsed(result: &Value) -> Value {
    serde_json::from_str(text(result)).expect("text is one JSON document")
}

/// The text is minified JSON and there is no second `structuredContent` copy.
#[tokio::test]
async fn test_mcp_result_is_minified_text_without_structured_content() {
    let (dir, _) = seed();
    let result = rpc(
        &dir,
        "mentisdb_ranked_search",
        json!({"text": "nightly build server", "limit": 3}),
    )
    .await;
    assert!(result.get("structuredContent").is_none());
    assert!(!text(&result).contains("\n"), "no pretty-printing");
    assert_eq!(
        text(&result),
        parsed(&result).to_string(),
        "text is exactly the minified document"
    );
    assert!(parsed(&result)["results"].is_array());
}

/// Other tools are minified too.
#[tokio::test]
async fn test_other_tools_are_minified() {
    let (dir, _) = seed();
    let result = rpc(&dir, "mentisdb_head", json!({})).await;
    assert!(!text(&result).contains("\n"));
    assert!(parsed(&result).is_object());
}

/// Ranked search is lean without asking: no integrity or signing fields, no
/// plain provenance, no default role, a collapsed `successor_of`; the id,
/// content and type stay.
#[tokio::test]
async fn test_ranked_search_is_lean_by_default() {
    let (dir, ids) = seed();
    let result = rpc(
        &dir,
        "mentisdb_ranked_search",
        json!({"text": "nightly build server policy", "limit": 3}),
    )
    .await;
    let body = parsed(&result);
    let hit = &body["results"][0];
    let thought = &hit["thought"];
    assert_eq!(thought["id"], json!(ids[2]));
    assert_eq!(thought["thought_type"], json!("Decision"));
    assert!(thought["content"].as_str().unwrap().contains("revision 3"));
    for gone in [
        "hash",
        "prev_hash",
        "provenance",
        "role",
        "schema_version",
        "agent_name",
    ] {
        assert!(thought.get(gone).is_none(), "{gone} should be elided");
    }
    assert!(hit.get("chain_key").is_none(), "home chain is implied");
    assert_eq!(thought["successor_of"], json!([ids[1]]));
    assert!(
        thought.get("relations").is_none(),
        "the link successor_of names is not repeated"
    );
    assert!(
        hit["score"].as_f64().unwrap().to_string().len() <= 6,
        "rounded score"
    );
}

/// `compact: false` brings back the full record, still minified.
#[tokio::test]
async fn test_compact_false_returns_the_full_record() {
    let (dir, _) = seed();
    let result = rpc(
        &dir,
        "mentisdb_ranked_search",
        json!({"text": "nightly build server policy", "limit": 3, "compact": false}),
    )
    .await;
    assert!(!text(&result).contains("\n"));
    let thought = &parsed(&result)["results"][0]["thought"];
    assert!(thought["hash"].is_string());
    assert_eq!(thought["provenance"]["trust"], json!("untrusted_data"));
    assert!(parsed(&result)["results"][0]["score"]["lexical"].is_number());
}

/// A contested memory must still say so in the lean form, and its contester
/// stands in for it.
#[tokio::test]
async fn test_lean_hit_still_reports_a_contested_memory() {
    let (dir, _) = seed();
    let result = rpc(
        &dir,
        "mentisdb_ranked_search",
        json!({"text": "weekly report server schedule", "limit": 5}),
    )
    .await;
    let results = parsed(&result)["results"].clone();
    let contested = results
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["thought"]["version_status"] == json!("contested"))
        .expect("contested hit keeps version_status");
    assert!(contested["thought"]["successors"].is_array());
    let contester = results
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["thought"]["thought_type"] == json!("Correction"))
        .expect("the correction stands in");
    assert_eq!(
        contester["thought"]["successor_of"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// Lexical search is lean by default and drops `match_sources`.
#[tokio::test]
async fn test_lexical_search_is_lean_by_default() {
    let (dir, _) = seed();
    let lean = parsed(
        &rpc(
            &dir,
            "mentisdb_lexical_search",
            json!({"text": "nightly build server", "limit": 3}),
        )
        .await,
    );
    let hit = &lean["results"][0];
    assert!(hit.get("match_sources").is_none());
    assert!(hit["thought"].get("hash").is_none());
    assert!(hit["matched_terms"].is_array());
    let full = parsed(
        &rpc(
            &dir,
            "mentisdb_lexical_search",
            json!({"text": "nightly build server", "limit": 3, "compact": false}),
        )
        .await,
    );
    assert!(full["results"][0]["match_sources"].is_array());
    assert!(full["results"][0]["thought"]["hash"].is_string());
}

/// Context bundles drop the locator that repeats the thought's id and index.
#[tokio::test]
async fn test_context_bundles_are_lean_by_default() {
    let (dir, _) = seed();
    let body = parsed(
        &rpc(
            &dir,
            "mentisdb_context_bundles",
            json!({"text": "nightly build server", "limit": 2}),
        )
        .await,
    );
    let seed = &body["bundles"][0]["seed"];
    assert!(seed.get("locator").is_none());
    assert!(seed["thought"].get("hash").is_none());
    assert!(seed["thought"]["id"].is_string());
}

/// Standing context returns the rendered prompt once; `include_entries` brings
/// the structured list back.
#[tokio::test]
async fn test_standing_context_returns_the_prompt_once() {
    let (dir, _) = seed();
    let lean = parsed(&rpc(&dir, "mentisdb_standing_context", json!({})).await);
    assert!(lean.get("entries").is_none());
    assert!(lean["prompt"]
        .as_str()
        .unwrap()
        .contains("Never deploy the build server on Fridays"));
    let both = parsed(
        &rpc(
            &dir,
            "mentisdb_standing_context",
            json!({"include_entries": true}),
        )
        .await,
    );
    assert!(both["entries"].as_array().is_some_and(|e| !e.is_empty()));
    assert_eq!(both["prompt"], lean["prompt"]);
}

/// `/tools/execute` is not the MCP surface: it keeps returning the JSON object
/// and the full hit by default.
#[tokio::test]
async fn test_tools_execute_keeps_the_full_shape() {
    let (dir, _) = seed();
    let router = mcp_router(config(&dir));
    let request = Request::builder()
        .method("POST")
        .uri("/tools/execute")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"tool": "mentisdb_ranked_search",
                   "parameters": {"text": "nightly build server policy", "limit": 2}})
            .to_string(),
        ))
        .unwrap();
    let response = router.oneshot(with_peer(request)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let thought = &value["result"]["output"]["results"][0]["thought"];
    assert!(thought["hash"].is_string());
    assert!(thought["provenance"].is_object());
}
