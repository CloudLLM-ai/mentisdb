//! What `compact: true` must never take from a search hit: the trust and
//! version fields an agent needs to judge it, and the public/private boundary.
//!
//! `server_compact_search_tests.rs` proves compact is the full response minus
//! the documented fields over a generated corpus of public chains. This file
//! covers what that corpus cannot:
//!
//! * the per-hit `provenance` object (`segment`, `signed`, `signature`,
//!   `verified_agent`, `trust`) survives intact, because it is the defence
//!   against prompt injection through retrieved memories;
//! * a public reader gets exactly the public segment in compact form too, and
//!   an authorized reader (a signed `X-Mentis-Auth` proof) gets the private
//!   segment with its provenance;
//! * successor redirection (`successor_of`, `version_status`, `successors`,
//!   `current_version`) and duplicate collapse (`collapsed_duplicates`) are
//!   kept on the compact thought.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::read_auth::{fresh_nonce, sign_request};
use mentisdb::server::{mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;
use uuid::Uuid;

/// Thought fields compact search leaves out (mirrors the server's list).
const DROPPED_THOUGHT_FIELDS: [&str; 7] = [
    "hash",
    "prev_hash",
    "thought_signature",
    "signing_key_id",
    "schema_version",
    "agent_name",
    "agent_owner",
];

/// Keys of the `provenance` object every hit carries.
const PROVENANCE_KEYS: [&str; 6] = [
    "segment",
    "signed",
    "signature",
    "signature_version",
    "verified_agent",
    "trust",
];

const GOV_SEED: [u8; 32] = [7; 32];
const SECRET_WORDS: [&str; 2] = ["SECRETALPHA", "SECRETBETA"];

fn gov() -> SigningKey {
    SigningKey::from_bytes(&GOV_SEED)
}

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("mentisdb_compact_trust_{tag}_{}", Uuid::new_v4()))
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

fn set_mode(chain: &mut MentisDb, mode: AccessMode) {
    if !chain.agent_registry().agents.contains_key("gov") {
        chain
            .upsert_agent("gov", Some("gov"), None, None, None)
            .unwrap();
        let now = chrono::Utc::now().timestamp();
        let public = gov().verifying_key().to_bytes().to_vec();
        let payload = key_registration_payload(chain.chain_key(), "gov", "gov-1", &public, now);
        let proof = KeyRegistrationProof {
            registered_at_unix_secs: now,
            signature: gov().sign(&payload).to_bytes().to_vec(),
        };
        chain
            .add_agent_key("gov", "gov-1", PublicKeyAlgorithm::Ed25519, public, &proof)
            .unwrap();
    }
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let seq = chain.access_policy().seq.map_or(0, |s| s + 1);
    let record = PolicyRecord::new(
        seq,
        mode,
        vec![AllowedKey::new(
            "gov",
            "gov-1",
            &gov().verifying_key().to_bytes(),
        )],
        &prev,
    )
    .unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&gov().sign(&payload).to_bytes());
    chain
        .set_access_policy("gov", &record, "gov-1", signature)
        .unwrap();
}

fn signed(chain: &mut MentisDb, mut input: ThoughtInput) {
    let payload = thought_payload_v2(chain.chain_key(), "gov", &head(chain), &input);
    input.signing_key_id = Some("gov-1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&gov().sign(&payload).to_bytes()));
    chain.append_thought("gov", input).unwrap();
}

/// A chain with a private segment: 0-1 public, 2 record (private), 3-4
/// private and signed, 5 record (open, still private), 6-7 public.
struct World {
    config: MentisDbServiceConfig,
    dir: PathBuf,
    private_ids: Vec<Uuid>,
    private_hashes: Vec<String>,
}

fn world(tag: &str) -> World {
    let dir = temp_dir(tag);
    {
        let mut chain =
            MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary)
                .unwrap();
        chain
            .append(
                "pub",
                ThoughtType::Insight,
                "pelican migration notes are public",
            )
            .unwrap();
        let p1 = chain
            .append(
                "pub",
                ThoughtType::Decision,
                "we chose postgres for the pelican registry",
            )
            .unwrap()
            .id;
        set_mode(&mut chain, AccessMode::Allowlist);
        signed(
            &mut chain,
            ThoughtInput::new(
                ThoughtType::Insight,
                format!(
                    "{} zebra launch codes for the pelican project",
                    SECRET_WORDS[0]
                ),
            )
            .with_refs(vec![0])
            .with_relations(vec![ThoughtRelation::new(
                ThoughtRelationKind::RelatedTo,
                p1,
            )]),
        );
        let s3 = chain.thoughts()[3].id;
        signed(
            &mut chain,
            ThoughtInput::new(
                ThoughtType::Constraint,
                format!("{} never reveal the zebra plan", SECRET_WORDS[1]),
            )
            .with_relations(vec![ThoughtRelation::new(
                ThoughtRelationKind::RelatedTo,
                s3,
            )]),
        );
        set_mode(&mut chain, AccessMode::Open);
        signed(
            &mut chain,
            ThoughtInput::new(ThoughtType::Insight, "pelican follow up after the lockdown")
                .with_refs(vec![3, 1]),
        );
        chain
            .append("pub", ThoughtType::Insight, "another public pelican fact")
            .unwrap();
        chain.flush().unwrap();
    }
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    let hidden = [2usize, 3, 4, 5];
    World {
        config: MentisDbServiceConfig::new(dir.clone(), "default", StorageAdapterKind::Binary),
        dir,
        private_ids: hidden.iter().map(|i| chain.thoughts()[*i].id).collect(),
        private_hashes: hidden
            .iter()
            .map(|i| chain.thoughts()[*i].hash.clone())
            .collect(),
    }
}

async fn send(router: &Router, mut request: Request<Body>) -> (StatusCode, String) {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// POSTs `body` to `uri`, with a read proof by `gov` when `authorized`.
async fn post(router: &Router, uri: &str, body: &Value, authorized: bool) -> (String, Value) {
    let text = body.to_string();
    let mut request = Request::post(uri).header("content-type", "application/json");
    if authorized {
        let header = sign_request(
            &GOV_SEED,
            "gov",
            "gov-1",
            "POST",
            uri,
            text.as_bytes(),
            chrono::Utc::now().timestamp(),
            &fresh_nonce(),
        );
        request = request.header("x-mentis-auth", header);
    }
    let (status, raw) = send(router, request.body(Body::from(text)).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{uri} {body}: {raw}");
    let json = serde_json::from_str(&raw).expect("json response");
    (raw, json)
}

/// The compact form of a full thought: minus the dropped fields and every
/// top-level null or empty value.
fn expected_compact_thought(full: &Value) -> Value {
    let compact: Map<String, Value> = full
        .as_object()
        .expect("thought object")
        .iter()
        .filter(|(key, value)| {
            !DROPPED_THOUGHT_FIELDS.contains(&key.as_str())
                && !matches!(value, Value::Null)
                && **value != json!("")
                && **value != json!([])
                && **value != json!({})
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Value::Object(compact)
}

/// Asserts every compact hit is its full twin minus the dropped fields, with
/// the provenance object whole, and returns the number of hits checked.
fn assert_compact_matches_full(label: &str, full: &Value, compact: &Value) -> usize {
    let full_hits = full["results"].as_array().expect("full results");
    let compact_hits = compact["results"].as_array().expect("compact results");
    assert_eq!(full_hits.len(), compact_hits.len(), "{label}");
    for (f, c) in full_hits.iter().zip(compact_hits) {
        assert_eq!(
            expected_compact_thought(&f["thought"]),
            c["thought"],
            "{label}"
        );
        let provenance = &c["thought"]["provenance"];
        assert_eq!(provenance, &f["thought"]["provenance"], "{label}");
        let keys: Vec<&str> = provenance
            .as_object()
            .unwrap_or_else(|| panic!("{label}: compact hit lost its provenance: {c}"))
            .keys()
            .map(String::as_str)
            .collect();
        for key in PROVENANCE_KEYS {
            assert!(keys.contains(&key), "{label}: provenance lost {key}: {c}");
        }
        assert_eq!(provenance["trust"], "untrusted_data", "{label}");
    }
    full_hits.len()
}

fn assert_no_private_data(world: &World, label: &str, body: &str) {
    for word in SECRET_WORDS {
        assert!(!body.contains(word), "{label} leaked {word}: {body}");
    }
    for id in &world.private_ids {
        assert!(
            !body.contains(&id.to_string()),
            "{label} leaked a private id: {body}"
        );
    }
    for hash in &world.private_hashes {
        assert!(
            !body.contains(hash),
            "{label} leaked a private hash: {body}"
        );
    }
}

fn private_queries() -> Vec<(&'static str, Value)> {
    vec![
        (
            "/v1/ranked-search",
            json!({"chain_key": "c", "text": "zebra pelican registry"}),
        ),
        (
            "/v1/ranked-search",
            json!({"chain_key": "c", "text": "registry", "graph": {"max_depth": 4}}),
        ),
        (
            "/v1/federated-search",
            json!({"chain_keys": ["c"], "text": "zebra pelican"}),
        ),
        (
            "/v1/federated-search",
            json!({"chain_keys": ["c"], "text": "registry", "graph": {"max_depth": 4}}),
        ),
    ]
}

/// A public reader's compact response is the public segment only, hit for hit
/// the full public response minus the dropped fields, and every hit says it
/// is public.
#[tokio::test]
async fn public_reader_compact_search_shows_only_the_public_segment() {
    let world = world("public");
    let router = rest_router(world.config.clone());
    let mut checked = 0;
    for (uri, body) in private_queries() {
        let (_, full) = post(&router, uri, &body, false).await;
        let mut compact_body = body.clone();
        compact_body["compact"] = json!(true);
        let (compact_raw, compact) = post(&router, uri, &compact_body, false).await;
        assert_no_private_data(&world, &format!("{uri} {body}"), &compact_raw);
        checked += assert_compact_matches_full(&format!("{uri} {body}"), &full, &compact);
        for hit in compact["results"].as_array().unwrap() {
            assert_eq!(hit["thought"]["provenance"]["segment"], "public", "{hit}");
        }
    }
    assert!(checked >= 4, "queries returned too few hits: {checked}");
    let _ = std::fs::remove_dir_all(&world.dir);
}

/// The same holds over MCP: compact never widens what a public reader sees.
#[tokio::test]
async fn public_reader_compact_search_over_mcp_leaks_nothing() {
    let world = world("mcp");
    let mcp = mcp_router(world.config.clone());
    for (tool, parameters) in [
        (
            "mentisdb_ranked_search",
            json!({"chain_key": "c", "text": "zebra pelican registry", "compact": true,
                   "graph": {"max_depth": 4}}),
        ),
        (
            "mentisdb_federated_search",
            json!({"chain_keys": ["c"], "text": "zebra pelican", "compact": true}),
        ),
    ] {
        let call = json!({"tool": tool, "parameters": parameters});
        let (raw, response) = post(&mcp, "/tools/execute", &call, false).await;
        assert_no_private_data(&world, tool, &raw);
        let hits = response["result"]["output"]["results"].as_array().unwrap();
        assert!(!hits.is_empty(), "{tool} returned no hits");
        for hit in hits {
            assert_eq!(hit["thought"]["provenance"]["segment"], "public", "{hit}");
            assert_eq!(hit["thought"]["provenance"]["trust"], "untrusted_data");
        }
    }
    let _ = std::fs::remove_dir_all(&world.dir);
}

/// A reader with a valid proof from an allowed key gets the private segment in
/// compact form, and each private hit keeps the provenance that says it is
/// private, signed, verified and untrusted data, although its signature and
/// key id are dropped from the thought.
#[tokio::test]
async fn authorized_reader_compact_search_keeps_private_hits_and_their_provenance() {
    let world = world("authorized");
    let router = rest_router(world.config.clone());
    let query = json!({"chain_key": "c", "text": "zebra launch codes", "limit": 10});
    let (_, full) = post(&router, "/v1/ranked-search", &query, true).await;
    let mut compact_query = query.clone();
    compact_query["compact"] = json!(true);
    let (raw, compact) = post(&router, "/v1/ranked-search", &compact_query, true).await;

    assert!(raw.contains(SECRET_WORDS[0]), "private hit missing: {raw}");
    assert!(assert_compact_matches_full("authorized", &full, &compact) > 0);
    let private_hit = compact["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| {
            hit["thought"]["content"]
                .to_string()
                .contains(SECRET_WORDS[0])
        })
        .expect("the private thought is a hit");
    let thought = &private_hit["thought"];
    assert!(thought.get("thought_signature").is_none(), "{thought}");
    assert!(thought.get("signing_key_id").is_none(), "{thought}");
    assert_eq!(thought["provenance"]["segment"], "private");
    assert_eq!(thought["provenance"]["signed"], true);
    assert_eq!(thought["provenance"]["signature"], "valid");
    assert_eq!(thought["provenance"]["verified_agent"], "gov");
    assert_eq!(thought["provenance"]["trust"], "untrusted_data");
    let _ = std::fs::remove_dir_all(&world.dir);
}

/// An unsigned thought's provenance carries its `null` fields into the compact
/// form: only top-level nulls are removed, never the ones inside `provenance`.
#[tokio::test]
async fn compact_keeps_null_fields_inside_provenance() {
    let world = world("nulls");
    let router = rest_router(world.config.clone());
    let (_, compact) = post(
        &router,
        "/v1/ranked-search",
        &json!({"chain_key": "c", "text": "pelican migration notes", "compact": true}),
        false,
    )
    .await;
    let hit = &compact["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hit| hit["thought"]["index"] == 0)
        .expect("the unsigned first thought is a hit")["thought"];
    assert_eq!(hit["provenance"]["signed"], false, "{hit}");
    assert_eq!(hit["provenance"]["signature"], "unsigned");
    assert!(hit["provenance"]["verified_agent"].is_null());
    assert!(hit["provenance"]
        .as_object()
        .unwrap()
        .contains_key("verified_agent"));
    let _ = std::fs::remove_dir_all(&world.dir);
}

const OLD_POLICY: &str = "Release policy: deploy to production on Fridays.";
const NEW_POLICY: &str = "Release policy: never deploy to production on Fridays.";

async fn append(router: &Router, body: Value) -> Value {
    post(router, "/v1/thoughts", &body, false).await.1
}

/// Ranked search in both forms over REST and MCP, for each chain key.
async fn search_both_forms(
    rest: &Router,
    mcp: &Router,
    body: Value,
) -> Vec<(&'static str, Value, Value)> {
    let mut compact_body = body.clone();
    compact_body["compact"] = json!(true);
    let (_, rest_full) = post(rest, "/v1/ranked-search", &body, false).await;
    let (_, rest_compact) = post(rest, "/v1/ranked-search", &compact_body, false).await;
    let tool =
        |parameters: &Value| json!({"tool": "mentisdb_ranked_search", "parameters": parameters});
    let (_, mcp_full) = post(mcp, "/tools/execute", &tool(&body), false).await;
    let (_, mcp_compact) = post(mcp, "/tools/execute", &tool(&compact_body), false).await;
    vec![
        ("rest", rest_full, rest_compact),
        (
            "mcp",
            mcp_full["result"]["output"].clone(),
            mcp_compact["result"]["output"].clone(),
        ),
    ]
}

/// Successor redirection must survive compact: the live successor ranks first
/// with `successor_of`, the superseded thought (asked for as history) keeps
/// `version_status`, `successors` and `current_version`, and a contested
/// thought keeps its `contests` successor.
#[tokio::test]
async fn compact_keeps_successor_and_contested_fields() {
    let dir = temp_dir("versions");
    let config = MentisDbServiceConfig::new(dir.clone(), "policy", StorageAdapterKind::Binary);
    let (rest, mcp) = (rest_router(config.clone()), mcp_router(config));
    let old = append(
        &rest,
        json!({"chain_key": "policy", "thought_type": "Decision", "content": OLD_POLICY}),
    )
    .await;
    let old_id = old["thought"]["id"].as_str().unwrap().to_string();
    let new = append(
        &rest,
        json!({"chain_key": "policy", "thought_type": "Decision", "content": NEW_POLICY,
               "relations": [{"kind": "supersedes", "target_id": old_id}]}),
    )
    .await;
    let new_id = new["thought"]["id"].as_str().unwrap().to_string();

    let redirected = json!({"chain_key": "policy", "text": OLD_POLICY, "limit": 5});
    for (transport, full, compact) in search_both_forms(&rest, &mcp, redirected).await {
        assert_compact_matches_full(transport, &full, &compact);
        let first = &compact["results"][0]["thought"];
        assert_eq!(first["id"], new_id.as_str(), "{transport}: {compact}");
        assert_eq!(first["successor_of"][0], old_id.as_str(), "{transport}");
    }

    let history = json!({"chain_key": "policy", "text": OLD_POLICY, "limit": 5,
                         "include_invalidated": true});
    for (transport, full, compact) in search_both_forms(&rest, &mcp, history).await {
        assert_compact_matches_full(transport, &full, &compact);
        let old_hit = compact["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["thought"]["id"] == old_id.as_str())
            .unwrap_or_else(|| panic!("{transport}: superseded thought missing: {compact}"));
        let thought = &old_hit["thought"];
        assert_eq!(thought["version_status"], "superseded", "{transport}");
        assert_eq!(thought["successors"][0]["thought_id"], new_id.as_str());
        assert_eq!(thought["current_version"]["kind"], "supersedes");
    }
    let _ = std::fs::remove_dir_all(&dir);

    // An untyped correction only contests: both stay visible, the old one flagged.
    let dir = temp_dir("contested");
    let config = MentisDbServiceConfig::new(dir.clone(), "policy", StorageAdapterKind::Binary);
    let (rest, mcp) = (rest_router(config.clone()), mcp_router(config));
    let old = append(
        &rest,
        json!({"chain_key": "policy", "thought_type": "Decision", "content": OLD_POLICY}),
    )
    .await;
    let old_id = old["thought"]["id"].as_str().unwrap().to_string();
    append(
        &rest,
        json!({"chain_key": "policy", "thought_type": "Correction", "content": NEW_POLICY,
               "refs": [0]}),
    )
    .await;
    let query = json!({"chain_key": "policy", "text": OLD_POLICY, "limit": 5});
    for (transport, full, compact) in search_both_forms(&rest, &mcp, query).await {
        assert_compact_matches_full(transport, &full, &compact);
        let old_hit = compact["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hit| hit["thought"]["id"] == old_id.as_str())
            .unwrap_or_else(|| panic!("{transport}: contested thought missing: {compact}"));
        assert_eq!(old_hit["thought"]["version_status"], "contested");
        assert_eq!(old_hit["thought"]["successors"][0]["kind"], "contests");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Near-duplicate collapse must survive compact: the surviving hit keeps the
/// ids of the copies folded into it.
#[tokio::test]
async fn compact_keeps_collapsed_duplicates() {
    let dir = temp_dir("collapse");
    let config = MentisDbServiceConfig::new(dir.clone(), "dup", StorageAdapterKind::Binary);
    let (rest, mcp) = (rest_router(config.clone()), mcp_router(config));
    for _ in 0..3 {
        append(
            &rest,
            json!({"chain_key": "dup", "thought_type": "Insight",
                   "content": "The staging database nightly backup runs at two in the morning."}),
        )
        .await;
    }
    let query = json!({"chain_key": "dup", "text": "staging database nightly backup", "limit": 5});
    for (transport, full, compact) in search_both_forms(&rest, &mcp, query).await {
        assert_compact_matches_full(transport, &full, &compact);
        let folded = compact["results"][0]["thought"]["collapsed_duplicates"]
            .as_array()
            .unwrap_or_else(|| panic!("{transport}: collapsed_duplicates missing: {compact}"));
        assert_eq!(folded.len(), 2, "{transport}: {compact}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Both search tools advertise `compact` and tell an agent that provenance is
/// kept, when to leave the flag off, and where the full record is.
#[tokio::test]
async fn mcp_tool_schema_describes_what_compact_keeps_and_when_to_skip_it() {
    let dir = temp_dir("schema");
    let mcp = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    let (_, listing) = post(&mcp, "/tools/list", &json!({}), false).await;
    let tools = listing["tools"]
        .as_array()
        .or(listing["result"]["tools"].as_array())
        .expect("tool list");
    for name in ["mentisdb_ranked_search", "mentisdb_federated_search"] {
        let tool = tools.iter().find(|t| t["name"] == name).expect(name);
        let schema = tool["parameters"].to_string();
        for needle in ["compact", "provenance", "mentisdb_get_thought", "verify"] {
            assert!(schema.contains(needle), "{name} schema lacks {needle}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `compact` has no effect on context bundles: the route shares the ranked
/// request type, so it accepts the field and keeps returning full thoughts.
/// This pins what the README and `docs/CLI_PARITY.md` say about it.
#[tokio::test]
async fn context_bundles_accept_compact_and_still_return_full_thoughts() {
    let world = world("bundles");
    let router = rest_router(world.config.clone());
    let query = json!({"chain_key": "c", "text": "pelican registry", "limit": 5});
    let mut compact_query = query.clone();
    compact_query["compact"] = json!(true);
    let (_, plain) = post(&router, "/v1/context-bundles", &query, false).await;
    let (_, compact) = post(&router, "/v1/context-bundles", &compact_query, false).await;
    assert_eq!(plain, compact);
    let seed = &compact["bundles"][0]["seed"]["thought"];
    assert!(seed.get("hash").is_some(), "{seed}");
    assert!(seed.get("agent_name").is_some(), "{seed}");
    let _ = std::fs::remove_dir_all(&world.dir);
}
