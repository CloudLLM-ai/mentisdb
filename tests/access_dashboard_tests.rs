#![cfg(feature = "server")]
//! Dashboard surface of chain access control: the policy is shown, and writes the
//! dashboard performs respect it with a proper status.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use dashmap::DashMap;
use ed25519_dalek::{Signer, SigningKey};
pub use mentisdb::access;
use mentisdb::access::{policy_thought_input, AccessMode, AllowedKey, PolicyRecord};
pub use mentisdb::auth;
pub use mentisdb::search;
use mentisdb::signing::{thought_payload_v2, StoredSignature};
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

fn router(dir: &PathBuf) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "default".to_string(),
        dashboard_pin: None,
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

async fn call(router: &axum::Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-mentisdb-dashboard", "1")
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

/// Build a chain on disk with an allowlist policy and one regular thought.
fn restricted_chain(name: &str) -> (PathBuf, SigningKey) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_access_dash_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "locked", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append("bot", ThoughtType::Insight, "before the policy")
        .unwrap();
    let key = SigningKey::from_bytes(&[1; 32]);
    let record = PolicyRecord::new(
        0,
        AccessMode::Allowlist,
        vec![AllowedKey::new(
            "bot",
            "k1",
            &key.verifying_key().to_bytes(),
        )],
        "",
    )
    .unwrap();
    let prev = chain.thoughts().last().unwrap().hash.clone();
    let payload = thought_payload_v2("locked", "bot", &prev, &policy_thought_input(&record));
    let signature = StoredSignature::encode_v2(&key.sign(&payload).to_bytes());
    chain
        .set_access_policy("bot", &record, "k1", signature)
        .unwrap();
    drop(chain);
    (dir, key)
}

/// The chain detail response carries the policy for the UI.
#[tokio::test]
async fn test_chain_detail_shows_the_access_policy() {
    let (dir, _) = restricted_chain("detail");
    let (status, body) = call(
        &router(&dir),
        "GET",
        "/dashboard/api/chains/locked",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["access_policy"]["mode"], "allowlist");
    assert_eq!(body["access_policy"]["keys"][0]["key_id"], "k1");
    assert_eq!(body["access_policy"]["seq"], 0);
    let _ = std::fs::remove_dir_all(dir);
}

/// Dashboard writes the policy forbids answer 403 with the reason and write nothing.
#[tokio::test]
async fn test_dashboard_writes_respect_the_policy() {
    let (dir, _) = restricted_chain("writes");
    let app = router(&dir);
    // Importing MEMORY.md would append unsigned thoughts.
    let (status, body) = call(
        &app,
        "POST",
        "/dashboard/api/chains/locked/import-markdown",
        json!({"markdown": "# MEMORY\n\n## Knowledge\n\n- [#1] FactLearned: x\n"}),
    )
    .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::BAD_REQUEST,
        "{status} {body}"
    );
    // Copying an agent's memories into the locked chain is refused too.
    let mut source =
        MentisDb::open_with_key_and_storage_kind(&dir, "open-source", StorageAdapterKind::Binary)
            .unwrap();
    source
        .append("copier", ThoughtType::Insight, "copy me")
        .unwrap();
    drop(source);
    let (status, body) = call(
        &app,
        "POST",
        "/dashboard/api/agents/open-source/copier/copy-to/locked",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("signature"),
        "{body}"
    );
    let reopened =
        MentisDb::open_with_key_and_storage_kind(&dir, "locked", StorageAdapterKind::Binary)
            .unwrap();
    assert_eq!(
        reopened.thoughts().len(),
        2,
        "genesis thought + policy record only"
    );
    let _ = std::fs::remove_dir_all(dir);
}
