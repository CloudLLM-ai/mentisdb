//! Bulk paths that bypass `append_thought` (merge, copy, backup restore) must
//! respect the chain access policy and must not choke on governance records.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::{
    policy_thought_input, AccessMode, AllowedKey, KeyRegistrationProof, PolicyRecord,
};
use mentisdb::backup::{create_backup, restore_backup, BackupOptions, RestoreOptions};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::signing::{key_registration_payload, thought_payload_v2, StoredSignature};
use mentisdb::{MentisDb, PublicKeyAlgorithm, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::util::ServiceExt;

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_sec_paths_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

fn head(chain: &MentisDb) -> String {
    chain
        .thoughts()
        .last()
        .map(|t| t.hash.clone())
        .unwrap_or_default()
}

/// Put `chain` under an allowlist policy governed by agent `gov` / key `k1`.
fn govern(chain: &mut MentisDb) {
    let secret = key();
    let public = secret.verifying_key().to_bytes().to_vec();
    chain
        .upsert_agent("gov", Some("gov"), None, None, None)
        .unwrap();
    let now = Utc::now().timestamp();
    let payload = key_registration_payload(chain.chain_key(), "gov", "k1", &public, now);
    let proof = KeyRegistrationProof {
        registered_at_unix_secs: now,
        signature: secret.sign(&payload).to_bytes().to_vec(),
    };
    chain
        .add_agent_key(
            "gov",
            "k1",
            PublicKeyAlgorithm::Ed25519,
            public.clone(),
            &proof,
        )
        .unwrap();
    let record = PolicyRecord::new(
        0,
        AccessMode::Allowlist,
        vec![AllowedKey::new("gov", "k1", &public)],
        "",
    )
    .unwrap();
    let payload = thought_payload_v2(
        chain.chain_key(),
        "gov",
        &head(chain),
        &policy_thought_input(&record),
    );
    let signature = StoredSignature::encode_v2(&secret.sign(&payload).to_bytes());
    chain
        .set_access_policy("gov", &record, "k1", signature)
        .unwrap();
}

/// Append a validly signed thought as the governor.
fn append_signed(chain: &mut MentisDb, text: &str) {
    let mut input = ThoughtInput::new(ThoughtType::Insight, text);
    let payload = thought_payload_v2(chain.chain_key(), "gov", &head(chain), &input);
    input.signing_key_id = Some("k1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key().sign(&payload).to_bytes()));
    chain.append_thought("gov", input).unwrap();
}

async fn merge(dir: &Path, source: &str, target: &str) -> (StatusCode, Value) {
    let config = MentisDbServiceConfig::new(dir.to_path_buf(), target, StorageAdapterKind::Binary);
    let app = rest_router(config);
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chains/merge")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"source_chain_key": source, "target_chain_key": target}).to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 1))));
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A governed source cannot be merged away. Merging would publish its private
/// thoughts to another chain, discard their signatures and delete the originals,
/// so it is refused up front and nothing is copied or deleted.
#[tokio::test]
async fn merging_a_governed_source_is_refused_and_leaves_both_chains_untouched() {
    let dir = fresh_dir("merge_src");
    let mut target = MentisDb::open_with_key(&dir, "target").unwrap();
    target
        .append("seed", ThoughtType::Finding, "target seed")
        .unwrap();
    drop(target);
    let mut source = MentisDb::open_with_key(&dir, "source").unwrap();
    source
        .append("seed", ThoughtType::Finding, "before policy")
        .unwrap();
    govern(&mut source);
    append_signed(&mut source, "after policy");
    let source_len = source.thoughts().len();
    drop(source);

    let (status, body) = merge(&dir, "source", "target").await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(body.to_string().contains("cannot merge"), "{body}");
    let target = MentisDb::open_with_key(&dir, "target").unwrap();
    assert_eq!(target.thoughts().len(), 1, "nothing was copied");
    let source = MentisDb::open_with_key(&dir, "source").unwrap();
    assert_eq!(source.thoughts().len(), source_len, "the source survives");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A signed/allowlist target refuses the unsigned copies; the merge must say so
/// before touching anything and keep the source.
#[tokio::test]
async fn merging_into_a_restricted_target_is_refused_up_front() {
    let dir = fresh_dir("merge_tgt");
    let mut target = MentisDb::open_with_key(&dir, "target").unwrap();
    govern(&mut target);
    let before = target.thoughts().len();
    drop(target);
    let mut source = MentisDb::open_with_key(&dir, "source").unwrap();
    source
        .append("seed", ThoughtType::Finding, "unsigned")
        .unwrap();
    drop(source);

    let (status, body) = merge(&dir, "source", "target").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(
        MentisDb::open_with_key(&dir, "target")
            .unwrap()
            .thoughts()
            .len(),
        before
    );
    assert_eq!(
        MentisDb::open_with_key(&dir, "source")
            .unwrap()
            .thoughts()
            .len(),
        1,
        "source must survive a refused merge"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn backup_of(dir: &Path) -> PathBuf {
    let out = dir.join(format!("{}.mentis", uuid::Uuid::new_v4()));
    create_backup(&BackupOptions {
        source_dir: dir.to_path_buf(),
        output_path: Some(out.clone()),
        flush_before_backup: true,
        include_tls: false,
    })
    .unwrap();
    out
}

fn chain_thought_counts(dir: &Path) -> Vec<usize> {
    let mut counts: Vec<usize> = mentisdb::load_registered_chains(dir)
        .unwrap()
        .chains
        .keys()
        .map(|key| MentisDb::open_with_key(dir, key).unwrap().thoughts().len())
        .collect();
    counts.sort_unstable();
    counts
}

/// Restoring a newer backup onto a governed chain splices raw records, so it
/// must not bypass the write policy: the suffix is fast-forwarded only after
/// every record in it verifies against the policy of its moment (here the
/// newer thought is validly signed by an allowed key); an open chain
/// fast-forwards as always. A suffix that does not verify is kept as a
/// separate renamed chain (see `tests/backup_governed_tests.rs`).
#[test]
fn restore_fast_forwards_a_governed_chain_only_after_verifying_the_suffix() {
    for governed in [true, false] {
        let origin = fresh_dir("bk_origin");
        let mut chain = MentisDb::open_with_key(&origin, "room").unwrap();
        chain.append("seed", ThoughtType::Finding, "base").unwrap();
        if governed {
            govern(&mut chain);
        }
        drop(chain);
        let older = backup_of(&origin);

        let mut chain = MentisDb::open_with_key(&origin, "room").unwrap();
        if governed {
            append_signed(&mut chain, "newer");
        } else {
            chain.append("seed", ThoughtType::Finding, "newer").unwrap();
        }
        drop(chain);
        let newer = backup_of(&origin);

        let replica = fresh_dir("bk_replica");
        restore_backup(older, replica.clone(), RestoreOptions { overwrite: false }).unwrap();
        let base_len = MentisDb::open_with_key(&replica, "room")
            .unwrap()
            .thoughts()
            .len();
        restore_backup(newer, replica.clone(), RestoreOptions { overwrite: false }).unwrap();
        let counts = chain_thought_counts(&replica);
        assert_eq!(counts, vec![base_len + 1], "{counts:?}");
        if governed {
            let restored = MentisDb::open_with_key(&replica, "room").unwrap();
            assert!(restored.verify_signatures().is_clean());
            assert!(restored.thoughts().iter().any(|t| t.content == "newer"));
        }
        let _ = std::fs::remove_dir_all(&origin);
        let _ = std::fs::remove_dir_all(&replica);
    }
}
