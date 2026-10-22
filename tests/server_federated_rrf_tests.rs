//! Federated ranked search over REST must fuse RRF with the same semantics as
//! `MentisDb::query_federated`: keyed by (chain, thought id), additive scaled
//! bonus, deterministic ordering.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

fn temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_fed_rrf_{tag}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

async fn post(router: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn seed(app: &axum::Router, chain: &str, contents: &[&str]) {
    for content in contents {
        let (status, _) = post(
            app.clone(),
            "/v1/thoughts",
            json!({"chain_key":chain,"agent_id":"t","thought_type":"Insight","content":content}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
}

async fn federated(app: &axum::Router, extra: Value) -> Vec<Value> {
    let mut body = json!({"chain_keys":["fed-a","fed-b"],"text":"quokka","limit":20});
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let (status, json) = post(app.clone(), "/v1/federated-search", body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json["results"].as_array().cloned().unwrap_or_default()
}

fn total(hit: &Value) -> f64 {
    hit["score"]["total"].as_f64().unwrap()
}

async fn seeded_app(tag: &str) -> (axum::Router, PathBuf) {
    let dir = temp_dir(tag);
    let app = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "fed-a",
        StorageAdapterKind::Binary,
    ));
    // Both chains share thought indices 0..N, which collided in the old
    // index-keyed RRF.
    seed(
        &app,
        "fed-a",
        &[
            "quokka",
            "garden notes about weather and soil",
            "quokka quokka quokka quokka quokka habitat quokka",
            "quokka diet and quokka behaviour",
            "a single quokka sighting at the shore",
            "quokka",
        ],
    )
    .await;
    seed(
        &app,
        "fed-b",
        &[
            "quokka",
            "quokka quokka tour guide",
            "unrelated harbour schedule",
            "quokka travel quokka",
        ],
    )
    .await;
    (app, dir)
}

/// With a small rerank window, the fused head must still contain the top
/// lexical hit instead of sinking below un-fused BM25-scale tail totals.
#[tokio::test]
async fn test_federated_rerank_keeps_top_lexical_hit_in_head() {
    let (app, dir) = seeded_app("head").await;
    let plain = federated(&app, json!({})).await;
    let reranked = federated(&app, json!({"enable_reranking":true,"rerank_k":2})).await;
    assert_eq!(plain.len(), reranked.len());
    assert_eq!(
        reranked[0]["thought"]["id"], plain[0]["thought"]["id"],
        "top lexical hit must stay first after fusion"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Totals are non-increasing and each (chain, id) appears exactly once, even
/// though both chains reuse the same thought indices.
#[tokio::test]
async fn test_federated_rerank_is_sorted_and_unique_across_chains() {
    let (app, dir) = seeded_app("sorted").await;
    let hits = federated(&app, json!({"enable_reranking":true,"rerank_k":3})).await;
    assert!(hits.windows(2).all(|w| total(&w[0]) >= total(&w[1])));
    let mut keys: Vec<(String, String)> = hits
        .iter()
        .map(|h| {
            (
                h["chain_key"].as_str().unwrap().to_string(),
                h["thought"]["id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), n);
    let chains: std::collections::BTreeSet<&str> = hits
        .iter()
        .filter_map(|h| h["chain_key"].as_str())
        .collect();
    assert_eq!(chains.len(), 2, "both chains contribute hits");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Paging over merged chains returns a full page when enough hits exist.
#[tokio::test]
async fn test_federated_page_is_full_across_chains() {
    let (app, dir) = seeded_app("page").await;
    let page = federated(&app, json!({"limit":4,"offset":3})).await;
    assert_eq!(page.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}
