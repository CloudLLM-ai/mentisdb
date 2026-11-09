//! A page must stay full when cross-chain redirection collapses hits.
//!
//! Thirty live facts on an ancestor chain are all replaced by one thought on
//! the branch (thirty cross-chain `Supersedes` relations). Every one of them
//! redirects to that single hit, so cutting each chain's page at
//! `offset + limit` before redirecting left the page with one hit while twenty
//! other live matches were waiting.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::Request;
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{
    MentisDb, StorageAdapterKind, ThoughtInput, ThoughtRelation, ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use tower::util::ServiceExt;

const QUERY: &str = "nightly archive volume snapshot";

/// Chain `base` (filler, thirty facts that match all four query words, twenty
/// that match three) and chain `branch` (branched from `base`, plus one thought
/// replacing all thirty facts).
fn seed() -> tempfile::TempDir {
    std::env::set_var("MENTISDB_EMBEDDINGS", "off");
    let dir = tempfile::tempdir().unwrap();
    let mut base =
        MentisDb::open_with_key_and_storage_kind(dir.path(), "base", StorageAdapterKind::Binary)
            .unwrap();
    for i in 0..300 {
        base.append_thought(
            "n",
            ThoughtInput::new(
                ThoughtType::Finding,
                format!("Gardening entry {i}: tomatoes."),
            ),
        )
        .unwrap();
    }
    let facts: Vec<_> = (0..30)
        .map(|i| {
            base.append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Insight,
                    format!("Archive volume {i}: nightly archive volume snapshot."),
                ),
            )
            .unwrap()
            .clone()
        })
        .collect();
    for i in 0..20 {
        base.append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Insight,
                format!("Spare note {i}: nightly archive volume."),
            ),
        )
        .unwrap();
    }
    let mut branch = base.branch_into(facts[0].id, "branch").unwrap();
    let relations = facts
        .iter()
        .map(|fact| ThoughtRelation {
            kind: ThoughtRelationKind::Supersedes,
            target_id: fact.id,
            chain_key: Some("base".into()),
            valid_at: None,
            invalid_at: None,
        })
        .collect();
    branch
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Consolidated storage plan for every backup.",
            )
            .with_relations(relations),
        )
        .unwrap();
    dir
}

async fn post(dir: &tempfile::TempDir, uri: &str, body: Value) -> Value {
    let router = rest_router(
        MentisDbServiceConfig::new(
            dir.path().to_path_buf(),
            "branch",
            StorageAdapterKind::Binary,
        )
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

/// Ranked search on the branch (which also reads its ancestor) returns a full,
/// duplicate-free page although thirty hits redirect to one.
#[tokio::test]
async fn test_ranked_page_stays_full_when_hits_redirect_to_one() {
    let dir = seed();
    let page = ids(&post(
        &dir,
        "/v1/ranked-search",
        json!({"chain_key": "branch", "text": QUERY, "limit": 5}),
    )
    .await);
    assert_eq!(page.len(), 5);
    assert_eq!(page.iter().collect::<HashSet<_>>().len(), 5);
}

/// Federated search over both chains does the same.
#[tokio::test]
async fn test_federated_page_stays_full_when_hits_redirect_to_one() {
    let dir = seed();
    let page = ids(&post(
        &dir,
        "/v1/federated-search",
        json!({"chain_keys": ["base", "branch"], "text": QUERY, "limit": 5}),
    )
    .await);
    assert_eq!(page.len(), 5);
    assert_eq!(page.iter().collect::<HashSet<_>>().len(), 5);
}

/// Paging through the redirected ranking tiles it: no repeat, no gap.
#[tokio::test]
async fn test_ranked_pages_tile_when_hits_redirect_to_one() {
    let dir = seed();
    let whole = ids(&post(
        &dir,
        "/v1/ranked-search",
        json!({"chain_key": "branch", "text": QUERY, "limit": 100}),
    )
    .await);
    let mut paged = Vec::new();
    for offset in (0..whole.len() + 5).step_by(5) {
        paged.extend(ids(&post(
            &dir,
            "/v1/ranked-search",
            json!({"chain_key": "branch", "text": QUERY, "offset": offset, "limit": 5}),
        )
        .await));
    }
    assert_eq!(paged, whole);
    assert_eq!(whole.iter().collect::<HashSet<_>>().len(), whole.len());
}
