#![cfg(feature = "server")]
//! Cross-chain and branch lineage obey the same rules, and report through the
//! same notes, as lineage inside one chain (see `lineage_trust_tests.rs`):
//!
//! * on an open chain a replacement written on another searched chain still
//!   redirects, and says who wrote it (`agent_id`, `trust`, `cross_agent`,
//!   `authorized`, the chain it lives on);
//! * on a governed chain a replacement whose writer is neither the target's
//!   agent nor an authorized key only contests: the target stays current and is
//!   reported `contested`, the link listed with `authorized: false`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::audience::Audience;
use mentisdb::lineage::{cross_chain_lineage_on, cross_chain_successor};
use mentisdb::read_auth::{fresh_nonce, sign_request};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{
    MentisDb, PublicKeyAlgorithm, StorageAdapterKind, Thought, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::util::ServiceExt;

const OLD: &str = "Release policy: we deploy to production on Fridays after lunch.";
/// Shares no words with [`OLD`].
const NEW: &str = "Ship changes only early in the week and never ahead of a weekend.";
const QUERY: &str = "release policy deploy to production on Fridays";
const GOV_SEED: [u8; 32] = [7; 32];
const B_SEED: [u8; 32] = [9; 32];

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "mentisdb_xtrust_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn open(dir: &Path, key: &str) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, key, StorageAdapterKind::Binary).unwrap()
}

fn key(seed: &[u8; 32]) -> SigningKey {
    SigningKey::from_bytes(seed)
}

fn cross_supersedes(content: &str, old: &Thought, chain: &str) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Decision, content).with_relations(vec![ThoughtRelation {
        kind: ThoughtRelationKind::Supersedes,
        target_id: old.id,
        chain_key: Some(chain.into()),
        valid_at: None,
        invalid_at: None,
    }])
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

fn register(chain: &mut MentisDb, agent: &str, key_id: &str, seed: &[u8; 32]) {
    chain
        .upsert_agent(agent, Some(agent), None, None, None)
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    let public = key(seed).verifying_key().to_bytes().to_vec();
    let payload = key_registration_payload(chain.chain_key(), agent, key_id, &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: key(seed).sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(agent, key_id, PublicKeyAlgorithm::Ed25519, public, &proof)
        .unwrap();
}

fn set_policy(chain: &mut MentisDb, allowed: &[(&str, &str, &[u8; 32])]) {
    let prev = chain
        .access_policy()
        .head_record_hash
        .clone()
        .unwrap_or_default();
    let seq = chain.access_policy().seq.map_or(0, |s| s + 1);
    let keys = allowed
        .iter()
        .map(|(agent, key_id, seed)| {
            AllowedKey::new(agent, key_id, &key(seed).verifying_key().to_bytes())
        })
        .collect();
    let record = PolicyRecord::new(seq, AccessMode::Allowlist, keys, &prev).unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&key(&GOV_SEED).sign(&payload).to_bytes());
    chain
        .set_access_policy("gov", &record, "gov-1", signature)
        .unwrap();
}

fn signed_as(
    chain: &mut MentisDb,
    agent: &str,
    key_id: &str,
    seed: &[u8; 32],
    mut input: ThoughtInput,
) -> Thought {
    let payload = thought_payload_v2(chain.chain_key(), agent, &head(chain), &input);
    input.signing_key_id = Some(key_id.into());
    input.thought_signature = Some(StoredSignature::encode_v2(
        &key(seed).sign(&payload).to_bytes(),
    ));
    chain.append_thought(agent, input).unwrap().clone()
}

/// `parent` (open, `gov` wrote the old policy) and `sibling` (open, `bob`
/// replaces it across chains).
fn open_world(name: &str) -> (PathBuf, Thought, Thought) {
    let dir = dir(name);
    let mut parent = open(&dir, "parent");
    let old = parent
        .append("gov", ThoughtType::Decision, OLD)
        .unwrap()
        .clone();
    parent.flush().unwrap();
    let mut sibling = open(&dir, "sibling");
    let new = sibling
        .append_thought("bob", cross_supersedes(NEW, &old, "parent"))
        .unwrap()
        .clone();
    sibling.flush().unwrap();
    (dir, old, new)
}

/// `sibling` is governed: `gov` and `b` are allowed, `b` replaces `gov`'s
/// decision on `parent`, and (when `revoke_b`) the operator then removes `b`.
fn governed_world(name: &str, revoke_b: bool) -> (PathBuf, Thought, Thought) {
    let dir = dir(name);
    let mut parent = open(&dir, "parent");
    let old = parent
        .append("gov", ThoughtType::Decision, OLD)
        .unwrap()
        .clone();
    parent.flush().unwrap();
    let mut sibling = open(&dir, "sibling");
    register(&mut sibling, "gov", "gov-1", &GOV_SEED);
    register(&mut sibling, "b", "b-1", &B_SEED);
    set_policy(
        &mut sibling,
        &[("gov", "gov-1", &GOV_SEED), ("b", "b-1", &B_SEED)],
    );
    let new = signed_as(
        &mut sibling,
        "b",
        "b-1",
        &B_SEED,
        cross_supersedes(NEW, &old, "parent"),
    );
    if revoke_b {
        set_policy(&mut sibling, &[("gov", "gov-1", &GOV_SEED)]);
    }
    sibling.flush().unwrap();
    (dir, old, new)
}

/// Federated search over `parent` and `sibling`; `read_as_gov` signs the request
/// with the governor's key so the private segment is visible.
async fn federated(dir: &Path, extra: Value, read_as_gov: bool) -> Vec<Value> {
    let router = rest_router(MentisDbServiceConfig::new(
        dir.to_path_buf(),
        "parent",
        StorageAdapterKind::Binary,
    ));
    let mut body = json!({"chain_keys": ["parent", "sibling"], "text": QUERY, "limit": 10});
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let text = body.to_string();
    let uri = "/v1/federated-search";
    let mut builder = Request::post(uri).header("content-type", "application/json");
    if read_as_gov {
        builder = builder.header(
            "x-mentis-auth",
            sign_request(
                &GOV_SEED,
                "gov",
                "gov-1",
                "POST",
                uri,
                text.as_bytes(),
                chrono::Utc::now().timestamp(),
                &fresh_nonce(),
            ),
        );
    }
    let mut request = builder.body(Body::from(text)).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            45100,
        ))));
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    json["results"].as_array().cloned().unwrap_or_default()
}

fn content_of(hit: &Value) -> &str {
    hit["thought"]["content"].as_str().unwrap_or_default()
}

// ---------------------------------------------------------------- open chains

/// A cross-chain replacement on open chains redirects and says who wrote it.
#[tokio::test]
async fn open_cross_chain_replacement_redirects_and_names_its_writer() {
    let (dir, old, new) = open_world("open_redirect");
    let hits = federated(&dir, json!({}), false).await;
    let top = &hits[0];
    assert_eq!(content_of(top), NEW, "{hits:?}");
    assert_eq!(top["chain_key"], "sibling");
    assert_eq!(top["thought"]["id"], new.id.to_string());
    assert_eq!(top["thought"]["successor_of"], json!([old.id]));
    assert_eq!(top["thought"]["successor_of_chain"], "parent");
    assert_eq!(
        top["thought"]["cross_agent"], true,
        "bob replaced gov's decision: {top}"
    );
}

/// The audit view carries the same note fields single-chain lineage does.
#[tokio::test]
async fn open_cross_chain_audit_view_carries_the_full_note() {
    let (dir, _old, new) = open_world("open_audit");
    let hits = federated(&dir, json!({"include_invalidated": true}), false).await;
    let old_hit = hits
        .iter()
        .find(|h| content_of(h) == OLD)
        .expect("the superseded thought is shown in audit mode");
    let thought = &old_hit["thought"];
    assert_eq!(thought["version_status"], "superseded");
    assert_eq!(thought["cross_agent"], true);
    let current = &thought["current_version"];
    assert_eq!(current["thought_id"], new.id.to_string());
    assert_eq!(current["agent_id"], "bob");
    assert_eq!(current["trust"], "unsigned");
    assert_eq!(current["cross_agent"], true);
    assert_eq!(current["authorized"], true);
    assert_eq!(current["chain_key"], "sibling");
    assert_eq!(thought["successors"], json!([current]));
}

/// The library function reports the same note.
#[test]
fn open_cross_chain_library_note_matches_single_chain_fields() {
    let (dir, old, new) = open_world("open_lib");
    let parent = open(&dir, "parent");
    let sibling = open(&dir, "sibling");
    let chains = [
        ("parent", &parent, Audience::Authorized),
        ("sibling", &sibling, Audience::Authorized),
    ];
    let found = cross_chain_successor(&chains, "parent", old.id, "gov").unwrap();
    assert_eq!(found.thought.id, new.id);
    assert_eq!(found.note.agent_id, "bob");
    assert!(found.note.cross_agent && found.note.authorized);
    assert_eq!(found.note.chain_key.as_deref(), Some("sibling"));
    let same_agent = cross_chain_successor(&chains, "parent", old.id, "bob").unwrap();
    assert!(!same_agent.note.cross_agent);
}

// ------------------------------------------------------------- governed chain

/// While the writer is authorized on the governed chain the replacement counts,
/// as it would inside one chain.
#[tokio::test]
async fn governed_cross_chain_authorized_writer_replaces() {
    let (dir, _old, new) = governed_world("gov_ok", false);
    let hits = federated(&dir, json!({}), true).await;
    assert_eq!(content_of(&hits[0]), NEW, "{hits:?}");
    assert_eq!(hits[0]["thought"]["id"], new.id.to_string());
    assert_eq!(hits[0]["thought"]["cross_agent"], true);
}

/// A writer who is no longer authorized on the governed chain, and is not the
/// target's agent, only contests: nothing is redirected and the target is
/// reported contested with the link marked unauthorized.
#[tokio::test]
async fn governed_cross_chain_unauthorized_writer_only_contests() {
    let (dir, old, new) = governed_world("gov_contest", true);
    let hits = federated(&dir, json!({}), true).await;
    let top = hits
        .iter()
        .find(|h| h["thought"]["id"] == old.id.to_string())
        .expect("the original stays in the results");
    assert_eq!(top["chain_key"], "parent");
    assert_eq!(content_of(top), OLD);
    // The link's own thought may match on its own chain, but it is never put in
    // the target's place.
    assert!(
        hits.iter()
            .all(|h| h["thought"].get("successor_of").is_none()),
        "the unauthorized replacement is not promoted over the original"
    );
    let thought = &top["thought"];
    assert_eq!(thought["version_status"], "contested");
    assert!(thought.get("successor_of").is_none());
    let note = &thought["successors"][0];
    assert_eq!(note["thought_id"], new.id.to_string());
    assert_eq!(note["agent_id"], "b");
    assert_eq!(note["authorized"], false);
    assert_eq!(note["cross_agent"], true);
    assert_eq!(note["chain_key"], "sibling");
    assert!(thought.get("current_version").is_none());

    // The audit view does not turn the contest into a replacement either.
    let audit = federated(&dir, json!({"include_invalidated": true}), true).await;
    let old_hit = audit
        .iter()
        .find(|h| h["thought"]["id"] == old.id.to_string())
        .unwrap();
    assert_eq!(old_hit["thought"]["version_status"], "contested");

    // The library agrees.
    let parent = open(&dir, "parent");
    let sibling = open(&dir, "sibling");
    let found = cross_chain_lineage_on(
        "sibling",
        &sibling,
        Audience::Authorized,
        "parent",
        old.id,
        "gov",
    );
    assert!(found.successor.is_none());
    assert_eq!(found.contests.len(), 1);
    assert!(!found.contests[0].authorized);
    let chains = [
        ("parent", &parent, Audience::Authorized),
        ("sibling", &sibling, Audience::Authorized),
    ];
    assert!(cross_chain_successor(&chains, "parent", old.id, "gov").is_none());
    // The same writer replacing their own thought is still allowed.
    let own = cross_chain_lineage_on(
        "sibling",
        &sibling,
        Audience::Authorized,
        "parent",
        old.id,
        "b",
    );
    assert_eq!(own.successor.unwrap().thought.id, new.id);
}
