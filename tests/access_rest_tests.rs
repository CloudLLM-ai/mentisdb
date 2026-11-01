#![cfg(feature = "server")]
//! REST and MCP surface of chain access control: signed appends, the policy
//! routes and tools, global-token-only policy changes, and verification.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{policy_thought_input, AccessMode, AllowedKey, PolicyRecord};
use mentisdb::server::{
    mcp_router, mcp_tool_auth, rest_route_auth, rest_router, MentisDbServiceConfig, OperationAuth,
};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_access_rest_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary)
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::post(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn secret(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Create the agent and register `key` for it through REST (with proof of possession).
async fn register(router: &Router, chain: &str, agent: &str, key_id: &str, key: &SigningKey) {
    let (status, _) = post(
        router,
        "/v1/agents/upsert",
        json!({"chain_key": chain, "agent_id": agent}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let public = key.verifying_key().to_bytes();
    let now = chrono::Utc::now().timestamp();
    let payload = key_registration_payload(chain, agent, key_id, &public, now);
    let (status, body) = post(
        router,
        "/v1/agents/keys",
        json!({
            "chain_key": chain, "agent_id": agent, "key_id": key_id, "algorithm": "ed25519",
            "public_key_hex": to_hex(&public), "registered_at_unix_secs": now,
            "registration_signature_hex": to_hex(&key.sign(&payload).to_bytes()),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// POST as the chain's administrator (agent `admin`, key `a1`, seed 1): the
/// head of a private chain is shown only to a key that proves it may read it.
async fn post_as_admin(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let text = body.to_string();
    let header = mentisdb::read_auth::sign_request(
        &[1u8; 32],
        "admin",
        "a1",
        "POST",
        uri,
        text.as_bytes(),
        chrono::Utc::now().timestamp(),
        &mentisdb::read_auth::fresh_nonce(),
    );
    let response = router
        .clone()
        .oneshot(
            Request::post(uri)
                .header("content-type", "application/json")
                .header("x-mentis-auth", header)
                .body(Body::from(text))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn head(router: &Router, chain: &str) -> String {
    let (_, body) = post_as_admin(router, "/v1/head", json!({"chain_key": chain})).await;
    body["head_hash"].as_str().unwrap_or("").to_string()
}

fn append_body(chain: &str, agent: &str, key_id: &str, signature: &[u8], content: &str) -> Value {
    json!({
        "chain_key": chain, "agent_id": agent, "thought_type": "Insight", "content": content,
        "signing_key_id": key_id, "thought_signature_hex": to_hex(signature),
    })
}

/// Sign "content" as an Insight at the chain's current head.
async fn signed_append(
    router: &Router,
    chain: &str,
    agent: &str,
    key_id: &str,
    key: &SigningKey,
    content: &str,
) -> (StatusCode, Value) {
    let prev = head(router, chain).await;
    let input = ThoughtInput::new(ThoughtType::Insight, content);
    let payload = thought_payload_v2(chain, agent, &prev, &input);
    let signature = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
    post(
        router,
        "/v1/thoughts",
        append_body(chain, agent, key_id, &signature, content),
    )
    .await
}

/// A signed append over REST is verified and stored; a forged one is a 403.
#[tokio::test]
async fn test_rest_append_verifies_signatures() {
    let router = rest_router(config("append"));
    let key = secret(1);
    register(&router, "c", "alice", "k1", &key).await;
    let (status, body) = signed_append(&router, "c", "alice", "k1", &key, "hello").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["thought"]["signing_key_id"], "k1");

    let (status, body) = signed_append(&router, "c", "alice", "k1", &secret(9), "forged").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let error = body["error"].as_str().unwrap();
    assert!(error.contains("signature verification failed"), "{error}");

    let (status, report) = post(&router, "/v1/access/verify", json!({"chain_key": "c"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            report["signed"].clone(),
            report["valid"].clone(),
            report["invalid"].clone()
        ),
        (json!(1), json!(1), json!(0))
    );
    assert_eq!(report["checks"][0]["version"], 2);
    assert_eq!(report["checks"][0]["status"], "valid");
}

/// Setting a policy over REST: show, enforce, and refuse loosening by a stranger.
#[tokio::test]
async fn test_rest_policy_lifecycle() {
    let router = rest_router(config("policy"));
    let (admin, stranger) = (secret(1), secret(2));
    register(&router, "c", "admin", "a1", &admin).await;
    register(&router, "c", "stranger", "s1", &stranger).await;

    let (status, shown) = post(&router, "/v1/access/policy", json!({"chain_key": "c"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["policy"]["mode"], "open");

    let admin_key = AllowedKey::new("admin", "a1", &admin.verifying_key().to_bytes());
    let record = PolicyRecord::new(0, AccessMode::Allowlist, vec![admin_key], "").unwrap();
    let prev = head(&router, "c").await;
    let payload = thought_payload_v2("c", "admin", &prev, &policy_thought_input(&record));
    let set = json!({
        "chain_key": "c", "agent_id": "admin", "signing_key_id": "a1",
        "signature_hex": to_hex(&admin.sign(&payload).to_bytes()), "record": record,
    });
    let (status, body) = post(&router, "/v1/access/policy/set", set.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy"]["mode"], "allowlist");
    assert_eq!(body["policy"]["seq"], 0);
    assert_eq!(body["thought"]["thought_type"], "AccessPolicy");

    // The same request again is out of sequence / stale.
    let (status, _) = post(&router, "/v1/access/policy/set", set).await;
    assert_ne!(status, StatusCode::OK);

    // Unsigned and non-listed writers are refused; the listed admin may write.
    let (status, _) = post(
        &router,
        "/v1/thoughts",
        json!({"chain_key": "c", "agent_id": "x", "thought_type": "Insight", "content": "no"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) =
        signed_append(&router, "c", "stranger", "s1", &stranger, "let me in").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("did not accept"));
    let (status, body) = signed_append(&router, "c", "admin", "a1", &admin, "welcome").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The reserved type cannot be appended directly.
    let (status, _) = post(&router, "/v1/thoughts", json!({"chain_key": "c", "agent_id": "admin", "thought_type": "AccessPolicy", "content": "{}"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The key list is shown only to a key that may read the chain.
    let (_, anonymous) = post(&router, "/v1/access/policy", json!({"chain_key": "c"})).await;
    assert_eq!(anonymous["policy"]["keys"], json!([]));
    assert_eq!(anonymous["policy"]["mode"], "allowlist");
    let (_, shown) = post_as_admin(&router, "/v1/access/policy", json!({"chain_key": "c"})).await;
    assert_eq!(shown["policy"]["keys"][0]["key_id"], "a1");
    assert_eq!(shown["policy"]["keys_root"].as_str().unwrap().len(), 64);
}

/// Changing the policy is a global-token operation; reads are chain scoped.
#[test]
fn test_policy_set_is_global_only() {
    assert_eq!(
        rest_route_auth("/v1/access/policy/set", "post"),
        OperationAuth::GlobalOnly
    );
    assert_eq!(
        mcp_tool_auth("mentisdb_set_access_policy"),
        OperationAuth::GlobalOnly
    );
    assert_eq!(
        rest_route_auth("/v1/access/policy", "post"),
        OperationAuth::ChainScoped
    );
    assert_eq!(
        mcp_tool_auth("mentisdb_get_access_policy"),
        OperationAuth::ChainScoped
    );
    assert_eq!(
        mcp_tool_auth("mentisdb_verify_signatures"),
        OperationAuth::ChainScoped
    );
}

/// The MCP tool list and dispatcher expose the access tools.
#[tokio::test]
async fn test_mcp_exposes_access_tools() {
    let router = mcp_router(config("mcp"));
    let (status, list) = post(&router, "/tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    for name in [
        "mentisdb_get_access_policy",
        "mentisdb_set_access_policy",
        "mentisdb_verify_signatures",
    ] {
        assert!(
            list["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == name),
            "{name}"
        );
    }
    let (status, result) = post(
        &router,
        "/tools/execute",
        json!({"tool": "mentisdb_bootstrap", "parameters": {"chain_key": "c", "content": "hi"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (status, result) = post(
        &router,
        "/tools/execute",
        json!({"tool": "mentisdb_get_access_policy", "parameters": {"chain_key": "c"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let (status, result) = post(
        &router,
        "/tools/execute",
        json!({"tool": "mentisdb_verify_signatures", "parameters": {"chain_key": "c"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
}

/// Merging into a chain whose policy needs signatures fails before anything is
/// copied, and the source chain is kept (merge cannot sign on anyone's behalf).
#[tokio::test]
async fn test_merge_into_a_restricted_chain_keeps_the_source() {
    let router = rest_router(config("merge"));
    let admin = secret(1);
    register(&router, "locked", "admin", "a1", &admin).await;
    let record = PolicyRecord::new(
        0,
        AccessMode::Signed,
        vec![AllowedKey::new(
            "admin",
            "a1",
            &admin.verifying_key().to_bytes(),
        )],
        "",
    )
    .unwrap();
    let prev = head(&router, "locked").await;
    let payload = thought_payload_v2("locked", "admin", &prev, &policy_thought_input(&record));
    let (status, body) = post(
        &router,
        "/v1/access/policy/set",
        json!({
            "chain_key": "locked", "agent_id": "admin", "signing_key_id": "a1",
            "signature_hex": to_hex(&admin.sign(&payload).to_bytes()), "record": record,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = post(&router, "/v1/thoughts", json!({"chain_key": "src", "agent_id": "bot", "thought_type": "Insight", "content": "keep me"})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post(
        &router,
        "/v1/chains/merge",
        json!({"source_chain_key": "src", "target_chain_key": "locked"}),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let (_, chains) = post_get(&router, "/v1/chains").await;
    assert!(
        chains["chain_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "src"),
        "the source chain must survive"
    );
    let (_, shown) = post_as_admin(&router, "/v1/head", json!({"chain_key": "locked"})).await;
    assert_eq!(shown["thought_count"], 1, "only the policy record");
}

async fn post_get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
