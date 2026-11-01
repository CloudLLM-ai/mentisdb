#![cfg(feature = "server")]
//! The public/private boundary on every REST and MCP read surface, plus the
//! read proof that lifts it for an authorized key.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::read_auth::{fresh_nonce, sign_request};
use mentisdb::server::{combined_mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Value};
use tower::util::ServiceExt;
use uuid::Uuid;

const GOV_SEED: [u8; 32] = [7; 32];
const SECRET_WORDS: [&str; 2] = ["SECRETALPHA", "SECRETBETA"];

fn gov() -> SigningKey {
    SigningKey::from_bytes(&GOV_SEED)
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

struct World {
    config: MentisDbServiceConfig,
    private_ids: Vec<Uuid>,
    private_hashes: Vec<String>,
}

/// 0-1 public, 2 record (private), 3-4 private, 5 record (open, still private), 6-7 public.
fn world(name: &str) -> World {
    let dir = std::env::temp_dir().join(format!("mentisdb_aud_srv_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
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
        config: MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary),
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

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, String) {
    send(
        router,
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// POST with a read proof made by `seed` for `agent`/`key_id`.
async fn post_as(
    router: &Router,
    uri: &str,
    body: Value,
    seed: &[u8; 32],
    agent: &str,
    key_id: &str,
) -> (StatusCode, String) {
    let text = body.to_string();
    let header = sign_request(
        seed,
        agent,
        key_id,
        "POST",
        uri,
        text.as_bytes(),
        chrono::Utc::now().timestamp(),
        &fresh_nonce(),
    );
    send(
        router,
        Request::post(uri)
            .header("content-type", "application/json")
            .header("x-mentis-auth", header)
            .body(Body::from(text))
            .unwrap(),
    )
    .await
}

fn assert_no_leak(world: &World, surface: &str, body: &str) {
    // The first public thought after a private segment names the hash of the
    // thought before it in `prev_hash`: that link is what keeps the chain
    // verifiable and is the one deliberate boundary hash (see CHAIN_ACCESS.md).
    let mut cleaned = body.to_string();
    for hash in &world.private_hashes {
        cleaned = cleaned.replace(
            &format!("\"prev_hash\":\"{hash}\""),
            "\"prev_hash\":\"<boundary>\"",
        );
    }
    let body = cleaned.as_str();
    for word in SECRET_WORDS {
        assert!(!body.contains(word), "{surface} leaked {word}: {body}");
    }
    for id in &world.private_ids {
        assert!(
            !body.contains(&id.to_string()),
            "{surface} leaked a private id: {body}"
        );
    }
    for hash in &world.private_hashes {
        assert!(
            !body.contains(hash),
            "{surface} leaked a private hash: {body}"
        );
    }
}

/// Every read route, asked without a proof, returns nothing from a private segment.
#[tokio::test]
async fn public_reader_sees_no_private_data_on_any_rest_read_route() {
    let world = world("rest_public");
    let router = rest_router(world.config.clone());
    let calls: Vec<(&str, Value)> = vec![
        ("/v1/search", json!({"chain_key":"c"})),
        ("/v1/search", json!({"chain_key":"c","text":"zebra"})),
        (
            "/v1/lexical-search",
            json!({"chain_key":"c","text":"zebra pelican"}),
        ),
        (
            "/v1/ranked-search",
            json!({"chain_key":"c","text":"zebra pelican registry"}),
        ),
        (
            "/v1/ranked-search",
            json!({"chain_key":"c","text":"registry","graph":{"max_depth":4}}),
        ),
        (
            "/v1/federated-search",
            json!({"chain_keys":["c"],"text":"zebra pelican"}),
        ),
        (
            "/v1/context-bundles",
            json!({"chain_key":"c","text":"zebra pelican","graph":{"max_depth":3}}),
        ),
        ("/v1/summary-candidates", json!({"chain_key":"c"})),
        ("/v1/recent-context", json!({"chain_key":"c","last_n":50})),
        ("/v1/standing-context", json!({"chain_key":"c"})),
        ("/v1/memory-markdown", json!({"chain_key":"c"})),
        ("/v1/thoughts/genesis", json!({"chain_key":"c"})),
        (
            "/v1/thoughts/traverse",
            json!({"chain_key":"c","chunk_size":50}),
        ),
        (
            "/v1/thoughts/traverse",
            json!({"chain_key":"c","direction":"backward","chunk_size":50}),
        ),
        ("/v1/head", json!({"chain_key":"c"})),
        ("/v1/agents", json!({"chain_key":"c"})),
        ("/v1/agent", json!({"chain_key":"c","agent_id":"gov"})),
        ("/v1/agent-registry", json!({"chain_key":"c"})),
        ("/v1/access/policy", json!({"chain_key":"c"})),
        ("/v1/access/verify", json!({"chain_key":"c"})),
    ];
    for (uri, body) in calls {
        let (status, text) = post(&router, uri, body.clone()).await;
        assert!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "{uri} {body}: {status} {text}"
        );
        assert_no_leak(&world, &format!("{uri} {body}"), &text);
    }
    // Lookups of private thoughts are indistinguishable from missing ones.
    for (index, id) in world.private_ids.iter().enumerate() {
        let hash = &world.private_hashes[index];
        let missing = post(
            &router,
            "/v1/thought",
            json!({"chain_key":"c","thought_id":Uuid::new_v4()}),
        )
        .await;
        for body in [
            json!({"chain_key":"c","thought_id":id}),
            json!({"chain_key":"c","thought_hash":hash}),
            json!({"chain_key":"c","thought_index":2 + index}),
        ] {
            let (status, text) = post(&router, "/v1/thought", body.clone()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{body}: {text}");
            assert_no_leak(&world, "/v1/thought", &text);
            assert_eq!(status, missing.0);
        }
    }
    // The chain listing counts only what a public reader can see.
    let (status, text) = send(
        &router,
        Request::get("/v1/chains").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let chains: Value = serde_json::from_str(&text).unwrap();
    let summary = chains["chains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["chain_key"] == "c")
        .unwrap();
    assert_eq!(summary["thought_count"], 4, "{text}");
    assert_eq!(summary["storage_location"], "");
    let (_, head) = post(&router, "/v1/head", json!({"chain_key":"c"})).await;
    let head: Value = serde_json::from_str(&head).unwrap();
    assert_eq!(head["thought_count"], 4);
    assert_eq!(head["latest_thought"]["index"], 7);
}

/// The same calls with a valid proof from an authorized key see the private segment.
#[tokio::test]
async fn authorized_key_reads_private_segments() {
    let world = world("rest_authorized");
    let router = rest_router(world.config.clone());
    for (uri, body) in [
        ("/v1/search", json!({"chain_key":"c","text":"SECRETALPHA"})),
        (
            "/v1/ranked-search",
            json!({"chain_key":"c","text":"zebra launch codes"}),
        ),
        (
            "/v1/lexical-search",
            json!({"chain_key":"c","text":"zebra"}),
        ),
        ("/v1/memory-markdown", json!({"chain_key":"c"})),
        ("/v1/thought", json!({"chain_key":"c","thought_index":3})),
    ] {
        let (status, text) = post_as(&router, uri, body.clone(), &GOV_SEED, "gov", "gov-1").await;
        assert_eq!(status, StatusCode::OK, "{uri}: {text}");
        assert!(
            text.contains("SECRETALPHA") || text.contains("SECRETBETA"),
            "{uri}: {text}"
        );
    }
    let (_, text) = post_as(
        &router,
        "/v1/head",
        json!({"chain_key":"c"}),
        &GOV_SEED,
        "gov",
        "gov-1",
    )
    .await;
    let head: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(head["thought_count"], 8);
    // Provenance tells the agent what it is looking at.
    let (_, text) = post_as(
        &router,
        "/v1/thought",
        json!({"chain_key":"c","thought_index":3}),
        &GOV_SEED,
        "gov",
        "gov-1",
    )
    .await;
    let thought: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(thought["thought"]["provenance"]["segment"], "private");
    assert_eq!(thought["thought"]["provenance"]["verified_agent"], "gov");
    assert_eq!(thought["thought"]["provenance"]["trust"], "untrusted_data");
}

/// A valid signature by a key that is not authorized gains nothing.
#[tokio::test]
async fn a_valid_proof_by_an_unauthorized_key_reads_public_only() {
    let world = world("rest_unauthorized");
    let router = rest_router(world.config.clone());
    let stranger = [9u8; 32];
    let (status, text) = post_as(
        &router,
        "/v1/search",
        json!({"chain_key":"c","text":"zebra"}),
        &stranger,
        "gov",
        "gov-1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_no_leak(&world, "stranger", &text);
    // Even naming the real agent and key id with a different public key.
    let (_, text) = post_as(
        &router,
        "/v1/memory-markdown",
        json!({"chain_key":"c"}),
        &stranger,
        "other",
        "k",
    )
    .await;
    assert_no_leak(&world, "stranger markdown", &text);
}

/// Replays, tampering and stale clocks are refused outright.
#[tokio::test]
async fn read_proofs_cannot_be_replayed_tampered_or_stale() {
    let world = world("rest_proofs");
    let router = rest_router(world.config.clone());
    let uri = "/v1/search";
    let body = json!({"chain_key":"c","text":"zebra"}).to_string();
    let now = chrono::Utc::now().timestamp();
    let make = |ts: i64, nonce: &str, body: &str| {
        sign_request(
            &GOV_SEED,
            "gov",
            "gov-1",
            "POST",
            uri,
            body.as_bytes(),
            ts,
            nonce,
        )
    };
    let request = |header: String, body: &str| {
        Request::post(uri)
            .header("content-type", "application/json")
            .header("x-mentis-auth", header)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let nonce = fresh_nonce();
    let header = make(now, &nonce, &body);
    let (status, text) = send(&router, request(header.clone(), &body)).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("SECRETALPHA"));
    // Same proof again: replay.
    let (status, _) = send(&router, request(header.clone(), &body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Same proof, different body: tampering.
    let other = json!({"chain_key":"c","text":"pelican"}).to_string();
    let (status, _) = send(&router, request(make(now, &fresh_nonce(), &body), &other)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Stale and future clocks.
    for ts in [now - 3600, now + 3600] {
        let (status, _) = send(&router, request(make(ts, &fresh_nonce(), &body), &body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // Garbage header.
    let (status, _) = send(&router, request("v1 nonsense".into(), &body)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Without the header the same request is simply public.
    let (status, text) = post(&router, uri, serde_json::from_str(&body).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_leak(&world, "no header", &text);
}

/// MCP tools and the legacy `/tools/execute` route apply the same boundary.
#[tokio::test]
async fn mcp_tools_apply_the_same_boundary() {
    let world = world("mcp");
    let router = combined_mcp_router(world.config.clone(), "127.0.0.1".parse().unwrap());
    let legacy = |tool: &str, params: Value| json!({"tool": tool, "parameters": params});
    for (tool, params) in [
        ("mentisdb_search", json!({"chain_key":"c","text":"zebra"})),
        (
            "mentisdb_ranked_search",
            json!({"chain_key":"c","text":"zebra pelican"}),
        ),
        (
            "mentisdb_recent_context",
            json!({"chain_key":"c","last_n":50}),
        ),
        ("mentisdb_memory_markdown", json!({"chain_key":"c"})),
        (
            "mentisdb_get_thought",
            json!({"chain_key":"c","thought_index":3}),
        ),
        ("mentisdb_head", json!({"chain_key":"c"})),
        ("mentisdb_list_agent_registry", json!({"chain_key":"c"})),
    ] {
        let (status, text) = post(&router, "/tools/execute", legacy(tool, params)).await;
        assert!(!status.is_server_error(), "{tool}: {status} {text}");
        assert_no_leak(&world, tool, &text);
    }
    // With a proof the same MCP call sees the private thought.
    let (status, text) = post_as(
        &router,
        "/tools/execute",
        legacy(
            "mentisdb_search",
            json!({"chain_key":"c","text":"SECRETALPHA"}),
        ),
        &GOV_SEED,
        "gov",
        "gov-1",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("SECRETALPHA"), "{text}");
    // And over the streamable-HTTP JSON-RPC root.
    let rpc = |name: &str, args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
    let (_, text) = post(
        &router,
        "/",
        rpc("mentisdb_search", json!({"chain_key":"c","text":"zebra"})),
    )
    .await;
    assert_no_leak(&world, "jsonrpc public", &text);
    let (_, text) = post_as(
        &router,
        "/",
        rpc(
            "mentisdb_search",
            json!({"chain_key":"c","text":"SECRETALPHA"}),
        ),
        &GOV_SEED,
        "gov",
        "gov-1",
    )
    .await;
    assert!(text.contains("SECRETALPHA"), "jsonrpc with proof: {text}");
}

/// The only MCP resource is the static skill text: no chain data.
#[tokio::test]
async fn mcp_resources_carry_no_chain_data() {
    let world = world("mcp_resources");
    let router = combined_mcp_router(world.config.clone(), "127.0.0.1".parse().unwrap());
    for method in ["resources/list", "resources/read"] {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":{"uri":"mentisdb://skill/core"}});
        let (_, text) = post(&router, "/", body).await;
        assert_no_leak(&world, method, &text);
    }
}

/// Branching from a private thought is refused exactly like an unknown thought.
#[tokio::test]
async fn branching_from_a_private_thought_is_not_found() {
    let world = world("branch");
    let router = rest_router(world.config.clone());
    let attempt = |id: Uuid, key: &str| json!({"source_chain_key":"c","branch_chain_key":key,"branch_thought_id":id});
    let unknown = Uuid::new_v4();
    let (status_unknown, text_unknown) =
        post(&router, "/v1/chains/branch", attempt(unknown, "b0")).await;
    for (n, id) in world.private_ids.iter().enumerate() {
        let (status, text) = post(
            &router,
            "/v1/chains/branch",
            attempt(*id, &format!("b{n}x")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
        assert_eq!(status, status_unknown);
        // Same message once the id the caller supplied is masked.
        assert_eq!(
            text.replace(&id.to_string(), "<id>"),
            text_unknown.replace(&unknown.to_string(), "<id>")
        );
    }
}

/// A chain with private segments cannot be merged away.
#[tokio::test]
async fn merging_a_governed_chain_is_refused() {
    let world = world("merge");
    let router = rest_router(world.config.clone());
    post(
        &router,
        "/v1/thoughts",
        json!({"chain_key":"t","agent_id":"x","thought_type":"Insight","content":"target"}),
    )
    .await;
    let (status, text) = post(
        &router,
        "/v1/chains/merge",
        json!({"source_chain_key":"c","target_chain_key":"t"}),
    )
    .await;
    assert!(status.is_client_error(), "{status} {text}");
    assert!(text.contains("cannot merge"), "{text}");
    // The source is still there.
    let (status, _) = post(&router, "/v1/head", json!({"chain_key":"c"})).await;
    assert_eq!(status, StatusCode::OK);
}

/// A refused write to a private chain does not say whether the agent or key
/// exists, was revoked or is merely not allowed: an unauthenticated caller
/// could otherwise enumerate the registry and the allowed key set.
#[tokio::test]
async fn refused_writes_do_not_reveal_registry_or_allowlist_state() {
    let world = world("probe");
    let router = rest_router(world.config.clone());
    let attempts: Vec<Value> = vec![
        // Unregistered agent, unknown key, real agent with a bogus signature,
        // and the real key signing for a stale head.
        json!({"chain_key":"c","agent_id":"nobody","thought_type":"Insight","content":"x","signing_key_id":"k","thought_signature_hex":"02".to_string() + &"00".repeat(64)}),
        json!({"chain_key":"c","agent_id":"gov","thought_type":"Insight","content":"x","signing_key_id":"nope","thought_signature_hex":"02".to_string() + &"00".repeat(64)}),
        json!({"chain_key":"c","agent_id":"gov","thought_type":"Insight","content":"x","signing_key_id":"gov-1","thought_signature_hex":"02".to_string() + &"11".repeat(64)}),
        json!({"chain_key":"c","agent_id":"gov","thought_type":"Insight","content":"x"}),
    ];
    // The chain is public again after the record at index 5, so unsigned
    // writes work; make it private again through the library to test refusals.
    {
        let mut chain = MentisDb::open_with_key_and_storage_kind(
            &world.config.chain_dir,
            "c",
            StorageAdapterKind::Binary,
        )
        .unwrap();
        set_mode(&mut chain, AccessMode::Allowlist);
        chain.flush().unwrap();
    }
    let router_private = rest_router(world.config.clone());
    drop(router);
    let mut messages = std::collections::BTreeSet::new();
    for body in attempts {
        let (status, text) = post(&router_private, "/v1/thoughts", body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}: {text}");
        let reason = serde_json::from_str::<Value>(&text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_string();
        messages.insert(reason);
    }
    // Every refusal reads the same to an anonymous caller. (An unsigned write
    // is "signature required", which names no registry state either.)
    assert!(messages.len() <= 2, "{messages:?}");
    for message in &messages {
        for probe in [
            "registered",
            "revoked",
            "allowed key set",
            "no key",
            "stale",
        ] {
            assert!(!message.contains(probe), "{message}");
        }
    }
}

/// A peer that keeps sending invalid read proofs is cut off before the server
/// spends more verification work on it; other peers are unaffected.
#[tokio::test]
async fn invalid_read_proofs_are_rate_limited_per_peer() {
    let world = world("ratelimit");
    let router = rest_router(world.config.clone());
    let bad =
        |ip: [u8; 4]| {
            let mut request = Request::post("/v1/search")
                .header("content-type", "application/json")
                .header("x-mentis-auth", "v1 garbage")
                .body(Body::from("{}"))
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                std::net::SocketAddr::from((ip, 50000)),
            ));
            request
        };
    let attacker = [203, 0, 113, 77];
    let mut statuses = Vec::new();
    for _ in 0..40 {
        let response = router.clone().oneshot(bad(attacker)).await.unwrap();
        statuses.push(response.status());
    }
    assert_eq!(statuses[0], StatusCode::UNAUTHORIZED);
    assert_eq!(
        *statuses.last().unwrap(),
        StatusCode::TOO_MANY_REQUESTS,
        "{statuses:?}"
    );
    // Another peer still gets the ordinary answer, and so does a request with no proof.
    let other = router
        .clone()
        .oneshot(bad([203, 0, 113, 78]))
        .await
        .unwrap();
    assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    let mut plain = Request::post("/v1/search")
        .header("content-type", "application/json")
        .body(Body::from(json!({"chain_key":"c"}).to_string()))
        .unwrap();
    plain
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            attacker, 50001,
        ))));
    let (status, _) = send(&router, plain).await;
    assert_eq!(status, StatusCode::OK, "no proof means public, not blocked");
}
