#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{body::Body, http::Request};
use dashmap::DashMap;
pub use mentisdb::{
    deregister_chain, load_registered_chains, AgentStatus, MentisDb, PublicKeyAlgorithm,
    SkillFormat, SkillRegistry, StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery,
    ThoughtRole, ThoughtType,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_dashboard_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn dashboard_router_for_dir(dir: &PathBuf) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: true,
    })
}

#[tokio::test]
async fn copy_to_chain_preserves_agent_description_for_detail_api() {
    let dir = unique_chain_dir();
    let mut source =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    source
        .upsert_agent(
            "astro",
            Some("Astro"),
            Some("@gubatron"),
            Some("Primary project manager agent."),
            Some(AgentStatus::Active),
        )
        .unwrap();
    source
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Seed the source chain."),
        )
        .unwrap();
    drop(source);

    let router = dashboard_router_for_dir(&dir);

    let copy = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/agents/source/astro/copy-to/target")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(copy.status(), axum::http::StatusCode::OK);

    let agent = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/agents/target/astro")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(agent.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(agent.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["display_name"], "Astro");
    assert_eq!(json["owner"], "@gubatron");
    assert_eq!(json["description"], "Primary project manager agent.");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn agent_detail_form_hydrates_values_after_dom_insertion() {
    let dir = unique_chain_dir();
    let router = dashboard_router_for_dir(&dir);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let html = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(html.to_vec()).unwrap();
    assert!(html.contains("<input type=\"text\" id=\"ad-name\">"));
    assert!(html.contains("<textarea id=\"ad-desc\"></textarea>"));
    assert!(html.contains("<input type=\"text\" id=\"ad-owner\">"));
    assert!(html.contains("document.getElementById('ad-name').value = agent.display_name || '';"));
    assert!(html.contains("document.getElementById('ad-desc').value = agent.description || '';"));
    assert!(html.contains("document.getElementById('ad-owner').value = agent.owner || '';"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_reads_latest_chain_and_agent_thoughts_without_restart() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "first thought"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);

    let initial_chain_thoughts = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/thoughts?per_page=10&page=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial_body = axum::body::to_bytes(initial_chain_thoughts.into_body(), usize::MAX)
        .await
        .unwrap();
    let initial_json: Value = serde_json::from_slice(&initial_body).unwrap();
    assert_eq!(initial_json["total"], 1);

    let mut reopened =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    reopened
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "second thought"),
        )
        .unwrap();
    drop(reopened);

    let chain_summary = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let summary_body = axum::body::to_bytes(chain_summary.into_body(), usize::MAX)
        .await
        .unwrap();
    let summary_json: Value = serde_json::from_slice(&summary_body).unwrap();
    let source_summary = summary_json
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["chain_key"] == "source")
        .unwrap();
    assert_eq!(source_summary["thought_count"], 2);

    let latest_agent_thoughts = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/agents/astro/thoughts?per_page=10&page=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let latest_body = axum::body::to_bytes(latest_agent_thoughts.into_body(), usize::MAX)
        .await
        .unwrap();
    let latest_json: Value = serde_json::from_slice(&latest_body).unwrap();
    assert_eq!(latest_json["total"], 2);
    assert_eq!(latest_json["thoughts"][0]["content"], "second thought");

    let _ = std::fs::remove_dir_all(&dir);
}
