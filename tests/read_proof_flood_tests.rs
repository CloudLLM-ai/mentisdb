#![cfg(feature = "server")]
//! The read-proof replay cache cannot be flooded with self-signed proofs, and
//! a proof refused by a full cache degrades to the public view instead of
//! failing the request.
//!
//! A proof verifies against the public key embedded in its own header, so any
//! fresh keypair makes a valid one. These tests pin that such proofs never
//! cost an honest, authorized reader anything.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::read_auth::{fresh_nonce, sign_request};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::json;
use tower::util::ServiceExt;

const SECRET: &str = "SECRETALPHA";

const GOV_SEED: [u8; 32] = [7; 32];

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

/// A chain with one public thought and one private (governed) thought.
fn router(name: &str) -> Router {
    let dir = std::env::temp_dir().join(format!("mentisdb_flood_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let mut chain =
            MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary)
                .unwrap();
        chain
            .append("pub", ThoughtType::Insight, "pelican notes are public")
            .unwrap();
        set_mode(&mut chain, AccessMode::Allowlist);
        signed(
            &mut chain,
            ThoughtInput::new(
                ThoughtType::Insight,
                format!("{SECRET} zebra launch codes for the pelican project"),
            ),
        );
        chain.flush().unwrap();
    }
    rest_router(MentisDbServiceConfig::new(
        dir,
        "default",
        StorageAdapterKind::Binary,
    ))
}

fn proof(seed: &[u8; 32], agent: &str, key_id: &str, uri: &str, body: &str) -> String {
    sign_request(
        seed,
        agent,
        key_id,
        "POST",
        uri,
        body.as_bytes(),
        chrono::Utc::now().timestamp(),
        &fresh_nonce(),
    )
}

async fn search(router: &Router, header: Option<&str>) -> (StatusCode, String) {
    let uri = "/v1/search";
    let body = json!({"chain_key":"c","text":"pelican"}).to_string();
    let mut request = Request::post(uri).header("content-type", "application/json");
    if let Some(header) = header {
        request = request.header("x-mentis-auth", header);
    }
    let mut request = request.body(Body::from(body)).unwrap();
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

fn search_proof(seed: &[u8; 32], agent: &str, key_id: &str) -> String {
    let body = json!({"chain_key":"c","text":"pelican"}).to_string();
    proof(seed, agent, key_id, "/v1/search", &body)
}

/// Regression: every self-signed proof used to be written into the replay
/// cache before any authorization, so enough of them made every honest proof
/// fail. Now they are served the public view and the honest reader is
/// untouched.
#[tokio::test]
async fn self_signed_proofs_do_not_disturb_an_authorized_reader() {
    let router = router("flood");
    for i in 0..300u32 {
        let mut seed = [0u8; 32];
        seed[..4].copy_from_slice(&i.to_le_bytes());
        seed[31] = 0xAA;
        let header = search_proof(&seed, "mallory", "m-1");
        let (status, text) = search(&router, Some(&header)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(
            !text.contains(SECRET),
            "an unregistered key saw private data"
        );
    }
    let (status, text) = search(&router, Some(&search_proof(&GOV_SEED, "gov", "gov-1"))).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(
        text.contains(SECRET),
        "the registered reader lost access: {text}"
    );
}

/// An unregistered key's proof grants nothing, so reusing it is not a replay
/// and is not refused either.
#[tokio::test]
async fn a_proof_that_grants_nothing_is_never_refused_as_a_replay() {
    let router = router("noreplay");
    let header = search_proof(&[0x42; 32], "mallory", "m-1");
    for _ in 0..3 {
        let (status, text) = search(&router, Some(&header)).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(!text.contains(SECRET));
    }
}

/// A proof that did grant access works once; reusing it is refused with 401
/// so the client learns why (unchanged behavior).
#[tokio::test]
async fn a_replayed_granting_proof_is_refused() {
    let router = router("replay");
    let header = search_proof(&GOV_SEED, "gov", "gov-1");
    let (status, text) = search(&router, Some(&header)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains(SECRET), "first use must authorize: {text}");
    let (status, text) = search(&router, Some(&header)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{text}");
    assert!(!text.contains(SECRET));
}

/// Grammar, clock and signature failures still answer 401 so a client with a
/// stale clock learns why.
#[tokio::test]
async fn invalid_proofs_are_still_refused_with_401() {
    let router = router("invalid");
    let (status, _) = search(&router, Some("v1 garbage")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body = json!({"chain_key":"c","text":"pelican"}).to_string();
    let stale = sign_request(
        &GOV_SEED,
        "gov",
        "gov-1",
        "POST",
        "/v1/search",
        body.as_bytes(),
        chrono::Utc::now().timestamp() - 3600,
        &fresh_nonce(),
    );
    let (status, _) = search(&router, Some(&stale)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
