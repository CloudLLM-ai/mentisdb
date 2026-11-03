#![cfg(feature = "server")]
//! A writer who has not proved read access must not be able to learn which
//! indices or ids belong to a private segment by pointing a thought at them.
//!
//! Library callers still get the specific `PrivateTarget` error (they are in
//! process); the REST handler answers an unauthorized writer with the generic
//! "not accepted", the same text every probe-sensitive refusal gets.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn gov() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
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

/// Index 0 public, 1 the policy record (private), 2 a signed private thought,
/// 3 the record that reopens the chain (private), then public again.
fn world() -> (Router, uuid::Uuid) {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_private_target_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let private_id;
    {
        let mut chain =
            MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary)
                .unwrap();
        chain
            .append("pub", ThoughtType::Insight, "public opening")
            .unwrap();
        set_mode(&mut chain, AccessMode::Allowlist);
        let mut input = ThoughtInput::new(ThoughtType::Insight, "SECRET private note");
        let payload = thought_payload_v2(chain.chain_key(), "gov", &head(&chain), &input);
        input.signing_key_id = Some("gov-1".into());
        input.thought_signature =
            Some(StoredSignature::encode_v2(&gov().sign(&payload).to_bytes()));
        chain.append_thought("gov", input).unwrap();
        set_mode(&mut chain, AccessMode::Open);
        chain
            .append("pub", ThoughtType::Insight, "public again")
            .unwrap();
        chain.flush().unwrap();
        private_id = chain.thoughts()[2].id;
    }
    (
        rest_router(MentisDbServiceConfig::new(
            dir,
            "default",
            StorageAdapterKind::Binary,
        )),
        private_id,
    )
}

async fn append(router: &Router, refs: Vec<u64>) -> (StatusCode, Value) {
    append_with(router, refs, json!([])).await
}

async fn append_with(router: &Router, refs: Vec<u64>, relations: Value) -> (StatusCode, Value) {
    let mut request = Request::post("/v1/thoughts")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "chain_key": "c", "agent_id": "pub", "thought_type": "Insight",
                "content": "probe", "refs": refs, "relations": relations
            })
            .to_string(),
        ))
        .unwrap();
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Regression: pointing an unsigned thought at index 2 answered "links to a
/// private segment", which told any caller that index 2 is private (and, by
/// contrast with a public index, which are not).
#[tokio::test]
async fn an_unauthorized_writer_cannot_tell_private_targets_from_missing_ones() {
    let (router, private_id) = world();

    // A public target works.
    let (status, body) = append(&router, vec![0]).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (private_status, private_body) = append(&router, vec![2]).await;
    let (missing_status, missing_body) = append(&router, vec![9999]).await;
    assert!(private_status.is_client_error(), "{private_body}");
    let text = private_body.to_string();
    assert!(!text.contains("private segment"), "{text}");
    assert!(text.contains("did not accept the write"), "{text}");
    // Indistinguishable from a target that does not exist.
    assert_eq!(private_status, missing_status);
    assert_eq!(private_body, missing_body);

    // The same for a relation target given by id.
    let relation = |id: uuid::Uuid| json!([{"kind": "RelatedTo", "target_id": id.to_string()}]);
    let (by_private, private_reply) = append_with(&router, vec![], relation(private_id)).await;
    let (by_missing, missing_reply) =
        append_with(&router, vec![], relation(uuid::Uuid::new_v4())).await;
    assert!(by_private.is_client_error(), "{private_reply}");
    assert_eq!(by_private, by_missing);
    assert_eq!(private_reply, missing_reply);
    assert!(!private_reply.to_string().contains("private segment"));
}

/// The in-process library error is unchanged for code that is not behind a
/// request boundary.
#[test]
fn the_library_still_reports_the_specific_error() {
    assert!(mentisdb::access::AccessError::PrivateTarget.is_probe_sensitive());
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_private_target_lib_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    chain
        .append("pub", ThoughtType::Insight, "public opening")
        .unwrap();
    set_mode(&mut chain, AccessMode::Allowlist);
    set_mode(&mut chain, AccessMode::Open);
    let error = chain
        .append_thought(
            "pub",
            ThoughtInput::new(ThoughtType::Insight, "probe").with_refs(vec![1]),
        )
        .expect_err("ref into a private segment");
    assert_eq!(
        mentisdb::access::AccessError::from_io(&error),
        Some(&mentisdb::access::AccessError::PrivateTarget)
    );
}
