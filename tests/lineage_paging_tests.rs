//! Paged REST search over contested thoughts: consecutive pages must tile the
//! ranking exactly (no repeated and no skipped hit).
//!
//! A contester stands in at its contested thought's score plus a small bump, so
//! page order used to differ from score order. The REST handlers fetch
//! `offset + page` hits, re-sort by score and skip `offset`, which then moved
//! contesters between windows: 30 contested decisions paged by 5 returned 60
//! hits of which only 44 were distinct.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::Request;
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::{json, Value};
use std::collections::HashSet;
use tower::util::ServiceExt;

const CHAIN: &str = "paging";
const QUERY: &str = "mango deploy rule";
const DECISIONS: usize = 30;

/// Filler (keeps the query terms under the document-frequency cutoff) plus
/// [`DECISIONS`] decisions, each contested by a correction that only
/// references it.
fn seed() -> tempfile::TempDir {
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let dir = tempfile::tempdir().unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir.path(), CHAIN, StorageAdapterKind::Binary)
            .unwrap();
    for i in 0..300 {
        chain
            .append_thought(
                "filler",
                ThoughtInput::new(
                    ThoughtType::Finding,
                    format!("Gardening entry {i}: tomatoes."),
                ),
            )
            .unwrap();
    }
    for i in 0..DECISIONS {
        let decision = chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Decision,
                    format!("Mango deploy rule {i}: deploy mango builds on day {i}."),
                ),
            )
            .unwrap()
            .clone();
        chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Correction,
                    format!("Correction {i}: the earlier rule was wrong, ship on another day."),
                )
                .with_refs(vec![decision.index]),
            )
            .unwrap();
    }
    dir
}

async fn post(dir: &tempfile::TempDir, uri: &str, body: Value) -> Value {
    let router = rest_router(
        MentisDbServiceConfig::new(dir.path().to_path_buf(), CHAIN, StorageAdapterKind::Binary)
            .with_bearer_token_access(false),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success(), "{uri}");
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn ids(page: &Value) -> Vec<String> {
    page["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["thought"]["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Concatenated pages of `size` equal one big page, with no duplicate.
async fn assert_pages_tile(dir: &tempfile::TempDir, uri: &str, extra: Value) {
    let request = |offset: usize, limit: usize| {
        let mut body = json!({"text": QUERY, "offset": offset, "limit": limit});
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    };
    let whole = ids(&post(dir, uri, request(0, 200)).await);
    assert!(
        whole.len() > DECISIONS,
        "{uri}: contesters surface ({})",
        whole.len()
    );
    let mut paged: Vec<String> = Vec::new();
    for offset in (0..whole.len() + 5).step_by(5) {
        paged.extend(ids(&post(dir, uri, request(offset, 5)).await));
    }
    let unique: HashSet<&String> = paged.iter().collect();
    assert_eq!(unique.len(), paged.len(), "{uri}: a hit was repeated");
    assert_eq!(paged, whole, "{uri}: pages must tile the ranking");
}

/// REST ranked search pages tile the ranking around contested thoughts.
#[tokio::test]
async fn test_ranked_pages_tile_around_contested_thoughts() {
    let dir = seed();
    assert_pages_tile(&dir, "/v1/ranked-search", json!({"chain_key": CHAIN})).await;
}

/// REST lexical search pages tile the ranking around contested thoughts.
#[tokio::test]
async fn test_lexical_pages_tile_around_contested_thoughts() {
    let dir = seed();
    assert_pages_tile(&dir, "/v1/lexical-search", json!({"chain_key": CHAIN})).await;
}

/// REST federated search pages tile the ranking around contested thoughts.
#[tokio::test]
async fn test_federated_pages_tile_around_contested_thoughts() {
    let dir = seed();
    assert_pages_tile(&dir, "/v1/federated-search", json!({"chain_keys": [CHAIN]})).await;
}
