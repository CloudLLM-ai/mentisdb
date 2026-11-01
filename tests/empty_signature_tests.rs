#![cfg(feature = "server")]
//! A zero-length `thought_signature` is a placeholder, not a signature.
//!
//! Real chains hold thoughts that older clients stored with an empty signature
//! field. They are unsigned: no key id, no registry entry, nothing to verify.
//! The verifier used to report each one as an invalid signature. These tests
//! build the same shape from synthetic content, so no user data is involved.

use std::io::Write;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::Request;
use mentisdb::access::SignatureStatus;
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind, Thought, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use tower::util::ServiceExt;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mentisdb_emptysig_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn open(dir: &Path) -> MentisDb {
    MentisDb::open_with_key_and_storage_kind(dir, "legacy", StorageAdapterKind::Binary).unwrap()
}

/// Write a chain whose stored thought carries `Some(vec![])`, as old clients did.
fn legacy_chain(dir: &Path) {
    {
        let mut chain = open(dir);
        chain
            .append("alice", ThoughtType::Insight, "plain")
            .unwrap();
        chain.flush().unwrap();
    }
    let (mut thought, path): (Thought, PathBuf) = {
        let chain = open(dir);
        (
            chain.thoughts().last().unwrap().clone(),
            PathBuf::from(chain.storage_location()),
        )
    };
    thought.prev_hash = thought.hash.clone();
    thought.index += 1;
    thought.id = uuid::Uuid::new_v4();
    thought.content = "placeholder signature".into();
    thought.thought_signature = Some(Vec::new());
    mentisdb::seal_thought_hash(&mut thought);
    let payload = bincode::serde::encode_to_vec(&thought, bincode::config::standard()).unwrap();
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&(payload.len() as u64).to_le_bytes())
        .unwrap();
    file.write_all(&payload).unwrap();
}

/// Regression: an empty stored signature is unsigned, not an invalid signature.
#[test]
fn stored_empty_signature_counts_as_unsigned() {
    let dir = dir("verify");
    legacy_chain(&dir);
    let chain = open(&dir);
    let stored = chain.thoughts().last().unwrap();
    assert_eq!(stored.thought_signature.as_deref(), Some(&[][..]));
    assert!(stored.signature_bytes().is_none());
    let report = chain.verify_signatures();
    assert_eq!((report.signed, report.valid, report.invalid), (0, 0, 0));
    assert!(report
        .checks
        .iter()
        .all(|c| matches!(c.status, SignatureStatus::Unsigned)));
    let provenance = chain.thought_json(stored);
    assert_eq!(provenance["provenance"]["signed"], false);
    assert_eq!(provenance["provenance"]["signature"], "unsigned");
}

/// A genuinely malformed non-empty signature is still reported invalid.
#[test]
fn malformed_signature_is_still_invalid() {
    let dir = dir("malformed");
    legacy_chain(&dir);
    let path = PathBuf::from(open(&dir).storage_location());
    let mut thought = open(&dir).thoughts().last().unwrap().clone();
    // Rewrite the last record with 5 junk bytes instead of an empty signature.
    let bytes = std::fs::read(&path).unwrap();
    let old = bincode::serde::encode_to_vec(&thought, bincode::config::standard()).unwrap();
    let keep = bytes.len() - old.len() - 8;
    thought.thought_signature = Some(vec![1, 2, 3, 4, 5]);
    mentisdb::seal_thought_hash(&mut thought);
    let new = bincode::serde::encode_to_vec(&thought, bincode::config::standard()).unwrap();
    let mut out = bytes[..keep].to_vec();
    out.extend_from_slice(&(new.len() as u64).to_le_bytes());
    out.extend_from_slice(&new);
    std::fs::write(&path, out).unwrap();
    let report = open(&dir).verify_signatures();
    assert_eq!(report.invalid, 1, "{:?}", report.checks);
}

/// Appending with an empty signature stores no signature at all.
#[test]
fn appending_an_empty_signature_stores_none() {
    let dir = dir("append");
    let mut chain = open(&dir);
    let mut input = ThoughtInput::new(ThoughtType::Insight, "placeholder");
    input.thought_signature = Some(Vec::new());
    let stored = chain.append_thought("alice", input).unwrap();
    assert!(stored.thought_signature.is_none());
}

/// REST: an empty array means "no signature supplied"; empty hex is a 400.
#[tokio::test]
async fn rest_empty_signature_fields_mean_unsigned() {
    let dir = dir("rest");
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    for (n, extra) in [
        json!({"thought_signature": []}),
        json!({"thought_signature": [], "thought_signature_hex": null}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut body = json!({"chain_key":"c","agent_id":"alice","thought_type":"Insight","content":format!("empty {n}")});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut request = Request::post("/v1/thoughts")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                40000,
            ))));
        let response = router.clone().oneshot(request).await.unwrap();
        assert!(
            response.status().is_success(),
            "{body}: {}",
            response.status()
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            value.to_string().contains("\"signed\":false"),
            "{body}: {value}"
        );
    }
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "c", StorageAdapterKind::Binary).unwrap();
    assert!(chain
        .thoughts()
        .iter()
        .all(|t| t.thought_signature.is_none()));
}
