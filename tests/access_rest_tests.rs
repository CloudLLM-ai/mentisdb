#![cfg(feature = "server")]
//! REST and MCP surface of chain access control: signed appends, reading the
//! policy, the operator-only boundary and verification.
//!
//! Chains are prepared the way an operator does it (library calls against the
//! data directory, the same code the CLI and dashboard run); REST and MCP are
//! used for what they may do: append, read and verify.

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::server::{
    mcp_router, mcp_tool_auth, rest_route_auth, rest_router, MentisDbServiceConfig, OperationAuth,
};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_access_rest_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn router_for(dir: &std::path::Path) -> Router {
    rest_router(MentisDbServiceConfig::new(
        dir.to_path_buf(),
        "default",
        StorageAdapterKind::Binary,
    ))
}

async fn raw(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    raw(
        router,
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

/// POST with a read proof made by `seed` for `agent` / `key_id`.
async fn post_as(
    router: &Router,
    uri: &str,
    body: Value,
    (seed, agent, key_id): (&[u8; 32], &str, &str),
) -> (StatusCode, Value) {
    let text = body.to_string();
    let header = mentisdb::read_auth::sign_request(
        seed,
        agent,
        key_id,
        "POST",
        uri,
        text.as_bytes(),
        chrono::Utc::now().timestamp(),
        &mentisdb::read_auth::fresh_nonce(),
    );
    raw(
        router,
        Request::post(uri)
            .header("content-type", "application/json")
            .header("x-mentis-auth", header)
            .body(Body::from(text))
            .unwrap(),
    )
    .await
}

fn secret(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// What an operator does with the CLI or dashboard: open the chain, register
/// the agent's key (vouched for by the operator) and, optionally, make it private.
fn prepare(dir: &std::path::Path, chain_key: &str, keys: &[(&str, &str, u8)], private: bool) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, chain_key, StorageAdapterKind::Binary)
            .unwrap();
    for (agent, key_id, seed) in keys {
        chain
            .add_agent_key_as_operator(
                agent,
                key_id,
                PublicKeyAlgorithm::Ed25519,
                secret(*seed).verifying_key().to_bytes().to_vec(),
            )
            .unwrap();
    }
    if private {
        apply(
            &mut chain,
            &operator,
            &OperatorOp::SetMode {
                mode: AccessMode::Allowlist,
                keys: vec![],
            },
        )
        .unwrap();
        for (agent, key_id, seed) in keys.iter().take(1) {
            apply(
                &mut chain,
                &operator,
                &OperatorOp::AllowKey {
                    agent_id: (*agent).into(),
                    key_id: (*key_id).into(),
                    public_key: to_hex(&secret(*seed).verifying_key().to_bytes()),
                },
            )
            .unwrap();
        }
    }
    chain.flush().unwrap();
}

/// The head of `chain` as the administrator (agent `admin`, key `a1`, seed 1)
/// sees it: a private chain shows its real head only to a key that may read it.
async fn head_as_admin(router: &Router, chain: &str) -> String {
    let (_, body) = post_as(
        router,
        "/v1/head",
        json!({"chain_key": chain}),
        (&[1u8; 32], "admin", "a1"),
    )
    .await;
    body["head_hash"].as_str().unwrap_or("").to_string()
}

/// Sign "content" as an Insight at the chain's current head and append it.
async fn signed_append(
    router: &Router,
    chain: &str,
    (agent, key_id, key): (&str, &str, &SigningKey),
    content: &str,
) -> (StatusCode, Value) {
    let prev = head_as_admin(router, chain).await;
    let input = ThoughtInput::new(ThoughtType::Insight, content);
    let payload = thought_payload_v2(chain, agent, &prev, &input);
    let signature = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
    post(
        router,
        "/v1/thoughts",
        json!({
            "chain_key": chain, "agent_id": agent, "thought_type": "Insight", "content": content,
            "signing_key_id": key_id, "thought_signature_hex": to_hex(&signature),
        }),
    )
    .await
}

/// A signed append over REST is verified and stored; a forged one is a 403.
#[tokio::test]
async fn test_rest_append_verifies_signatures() {
    let dir = dir("append");
    prepare(&dir, "c", &[("alice", "k1", 1)], false);
    let router = router_for(&dir);
    let key = secret(1);
    let (status, body) = signed_append(&router, "c", ("alice", "k1", &key), "hello").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["thought"]["signing_key_id"], "k1");
    assert_eq!(body["thought"]["provenance"]["verified_agent"], "alice");

    let (status, body) = signed_append(&router, "c", ("alice", "k1", &secret(9)), "forged").await;
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

/// A private chain over REST: enforce the allowed set, show the policy only to
/// readers who proved a key, and never let REST change it.
#[tokio::test]
async fn test_rest_policy_lifecycle() {
    let dir = dir("policy");
    // `admin` is allowed; `stranger` is registered but not allowed.
    prepare(
        &dir,
        "c",
        &[("admin", "a1", 1), ("stranger", "s1", 2)],
        true,
    );
    let router = router_for(&dir);
    let (admin, stranger) = (secret(1), secret(2));

    let (status, shown) = post(&router, "/v1/access/policy", json!({"chain_key": "c"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown["policy"]["mode"], "allowlist");
    assert_eq!(shown["policy"]["keys"], json!([]), "keys are not public");
    assert_eq!(shown["audience"], "public");

    // Unsigned writers, the reserved type and non-listed writers are refused.
    let (status, _) = post(
        &router,
        "/v1/thoughts",
        json!({"chain_key": "c", "agent_id": "x", "thought_type": "Insight", "content": "no"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) =
        signed_append(&router, "c", ("stranger", "s1", &stranger), "let me in").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("did not accept"));
    let (status, _) = post(&router, "/v1/thoughts", json!({"chain_key": "c", "agent_id": "admin", "thought_type": "AccessPolicy", "content": "{}"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The allowed administrator writes and reads.
    let (status, body) = signed_append(&router, "c", ("admin", "a1", &admin), "welcome").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, shown) = post_as(
        &router,
        "/v1/access/policy",
        json!({"chain_key": "c"}),
        (&[1u8; 32], "admin", "a1"),
    )
    .await;
    assert_eq!(shown["audience"], "authorized");
    assert!(shown["policy"]["keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["key_id"] == "a1"));
    assert_eq!(shown["policy"]["keys_root"].as_str().unwrap().len(), 64);

    // Not even the administrator can change the policy over REST.
    let (status, body) = post_as(
        &router,
        "/v1/access/policy/set",
        json!({"chain_key": "c"}),
        (&[1u8; 32], "admin", "a1"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// Changing the policy is not a token operation at all; reads are chain scoped.
#[test]
fn test_policy_set_is_global_only() {
    assert_eq!(
        rest_route_auth("/v1/access/policy/set", "post"),
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

/// The MCP tool list offers the read-only access tools and no administration.
#[tokio::test]
async fn test_mcp_exposes_access_tools() {
    let dir = dir("mcp");
    let router = mcp_router(MentisDbServiceConfig::new(
        dir,
        "default",
        StorageAdapterKind::Binary,
    ));
    let (status, list) = post(&router, "/tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for name in ["mentisdb_get_access_policy", "mentisdb_verify_signatures"] {
        assert!(names.contains(&name), "{name}");
    }
    assert!(!names.contains(&"mentisdb_set_access_policy"));
    let (status, result) = post(
        &router,
        "/tools/execute",
        json!({"tool": "mentisdb_bootstrap", "parameters": {"chain_key": "c", "content": "hi"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    for tool in ["mentisdb_get_access_policy", "mentisdb_verify_signatures"] {
        let (status, result) = post(
            &router,
            "/tools/execute",
            json!({"tool": tool, "parameters": {"chain_key": "c"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{tool}: {result}");
    }
}

/// Merging into a chain whose policy needs signatures fails before anything is
/// copied, and the source chain is kept (merge cannot sign on anyone's behalf).
#[tokio::test]
async fn test_merge_into_a_restricted_chain_keeps_the_source() {
    let dir = dir("merge");
    prepare(&dir, "locked", &[("admin", "a1", 1)], true);
    let router = router_for(&dir);
    let (status, _) = post(&router, "/v1/thoughts", json!({"chain_key": "src", "agent_id": "bot", "thought_type": "Insight", "content": "keep me"})).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post(
        &router,
        "/v1/chains/merge",
        json!({"source_chain_key": "src", "target_chain_key": "locked"}),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let response = router
        .clone()
        .oneshot(Request::get("/v1/chains").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let chains: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        chains["chain_keys"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "src"),
        "the source chain must survive"
    );
    let (_, shown) = post_as(
        &router,
        "/v1/head",
        json!({"chain_key": "locked"}),
        (&[1u8; 32], "admin", "a1"),
    )
    .await;
    // The operator's two records (mode, then the allowed key) and nothing else.
    assert_eq!(shown["thought_count"], 2, "only the policy records");
}
