#![cfg(feature = "server")]
//! A key id, once filed for an agent, is never rebound: the registration proof
//! shows possession of the NEW key only, so accepting it for an existing id
//! would let anyone replace a victim's key, or revive a revoked one. Checked
//! through the library and through the dashboard keys route.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use ed25519_dalek::{Signer, SigningKey};
pub use mentisdb::access;
use mentisdb::access::KeyRegistrationProof;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
pub use mentisdb::search;
use mentisdb::signing::{key_registration_payload, to_hex};
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, ManagedVectorProviderKind, MentisDb, PublicKeyAlgorithm,
    RankedSearchGraph, RankedSearchHit, RankedSearchQuery, SkillFormat, SkillRegistry, SkillUpload,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

const PIN: &str = "2468";
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn chain_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_keyovw_{label}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain
        .append("victim", ThoughtType::Insight, "hello")
        .unwrap();
    chain.flush().unwrap();
    dir
}

fn signer(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn proof_for(key: &SigningKey, agent: &str, key_id: &str) -> KeyRegistrationProof {
    let now = chrono::Utc::now().timestamp();
    let public = key.verifying_key().to_bytes();
    let payload = key_registration_payload("reg", agent, key_id, &public, now);
    KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key.sign(&payload).to_bytes().to_vec(),
    }
}

fn keys_of(dir: &Path, agent: &str) -> Vec<(String, String, bool)> {
    let chain =
        MentisDb::open_with_key_and_storage_kind(dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain
        .get_agent(agent)
        .unwrap()
        .public_keys
        .iter()
        .map(|k| {
            (
                k.key_id.clone(),
                to_hex(&k.public_key_bytes),
                k.revoked_at.is_some(),
            )
        })
        .collect()
}

/// Library: re-registering an id with another key (valid proof of that key) is
/// refused for an active and for a revoked id; the same active key again is a no-op.
#[test]
fn test_add_agent_key_never_rebinds_an_existing_id() {
    let dir = chain_dir("lib");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    let (victim, attacker) = (signer(1), signer(2));
    let victim_pub = victim.verifying_key().to_bytes().to_vec();
    let attacker_pub = attacker.verifying_key().to_bytes().to_vec();
    chain
        .add_agent_key(
            "victim",
            "v1",
            PublicKeyAlgorithm::Ed25519,
            victim_pub.clone(),
            &proof_for(&victim, "victim", "v1"),
        )
        .unwrap();

    let hijack = chain.add_agent_key(
        "victim",
        "v1",
        PublicKeyAlgorithm::Ed25519,
        attacker_pub.clone(),
        &proof_for(&attacker, "victim", "v1"),
    );
    assert!(hijack.is_err(), "active id must not be rebound");
    assert!(hijack.unwrap_err().to_string().contains("already bound"));
    assert!(
        chain
            .add_agent_key(
                "victim",
                "v1",
                PublicKeyAlgorithm::Ed25519,
                victim_pub.clone(),
                &proof_for(&victim, "victim", "v1")
            )
            .is_ok(),
        "the same key again is a no-op"
    );

    chain.revoke_agent_key("victim", "v1").unwrap();
    let revive = chain.add_agent_key(
        "victim",
        "v1",
        PublicKeyAlgorithm::Ed25519,
        attacker_pub,
        &proof_for(&attacker, "victim", "v1"),
    );
    assert!(
        revive.is_err(),
        "revoked id must not be revived with another key"
    );
    let revive_same = chain.add_agent_key(
        "victim",
        "v1",
        PublicKeyAlgorithm::Ed25519,
        victim_pub.clone(),
        &proof_for(&victim, "victim", "v1"),
    );
    assert!(revive_same.is_err(), "a revoked id stays revoked");
    chain.flush().unwrap();
    drop(chain);
    assert_eq!(
        keys_of(&dir, "victim"),
        vec![("v1".to_string(), to_hex(&victim_pub), true)]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn dashboard(dir: &Path, pin: Option<&str>) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.to_path_buf(),
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
    pin: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1");
    if let Some(pin) = pin {
        request = request.header("authorization", format!("Bearer {pin}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
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

fn keys_body(key: &SigningKey, key_id: &str) -> Value {
    let proof = proof_for(key, "victim", key_id);
    json!({
        "key_id": key_id,
        "algorithm": "ed25519",
        "public_key_hex": to_hex(&key.verifying_key().to_bytes()),
        "registered_at_unix_secs": proof.registered_at_unix_secs,
        "registration_signature_hex": to_hex(&proof.signature),
    })
}

const KEYS: &str = "/dashboard/api/agents/reg/victim/keys";

/// Dashboard route: the overwrite attempt is refused (4xx) for an active and a
/// revoked id, and the registry still holds only the victim's key, revoked state intact.
#[tokio::test]
async fn test_dashboard_keys_route_refuses_to_rebind_an_id() {
    let dir = chain_dir("dash");
    let router = dashboard(&dir, Some(PIN));
    let (victim, attacker) = (signer(3), signer(4));
    let (status, _) = call(&router, "POST", KEYS, keys_body(&victim, "v1"), Some(PIN)).await;
    assert_eq!(status, StatusCode::OK);

    let (status, reply) = call(&router, "POST", KEYS, keys_body(&attacker, "v1"), Some(PIN)).await;
    assert!(status.is_client_error(), "{status} {reply}");
    let (status, _) = call(
        &router,
        "DELETE",
        &format!("{KEYS}/v1"),
        Value::Null,
        Some(PIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, reply) = call(&router, "POST", KEYS, keys_body(&attacker, "v1"), Some(PIN)).await;
    assert!(status.is_client_error(), "revoked id: {status} {reply}");

    assert_eq!(
        keys_of(&dir, "victim"),
        vec![(
            "v1".to_string(),
            to_hex(&victim.verifying_key().to_bytes()),
            true
        )]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
