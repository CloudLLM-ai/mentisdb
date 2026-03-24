#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{body::Body, http::Request};
use dashmap::DashMap;
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, MentisDb, PublicKeyAlgorithm, SkillFormat, SkillRegistry,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRole, ThoughtType,
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

fn dashboard_router_with_chains(
    dir: &PathBuf,
    chains: Arc<DashMap<String, Arc<RwLock<MentisDb>>>>,
) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains,
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: true,
    })
}

fn dashboard_router_for_dir(dir: &PathBuf) -> axum::Router {
    dashboard_router_with_chains(dir, Arc::new(DashMap::new()))
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
async fn dashboard_chain_agent_counts_link_to_agent_sections() {
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
    assert!(html.contains("function agentListAnchorId(chainKey) {"));
    assert!(html.contains("function agentListHash(chainKey) {"));
    assert!(html.contains("else if (parts[0] === 'agents' && parts[1])             renderAgentList(decodeURIComponent(parts[1]));"));
    assert!(html.contains(
        r#"<td onclick="event.stopPropagation()"><a href="${agentListHash(c.chain_key)}""#
    ));
    assert!(html.contains(
        r#"<div class="section-label" id="${agentListAnchorId(ck)}"><a href="${agentListHash(ck)}""#
    ));
    assert!(html.contains("target.scrollIntoView({ behavior: 'auto', block: 'start' });"));

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

#[tokio::test]
async fn deleting_a_cached_chain_does_not_reregister_it_on_last_drop() {
    let dir = unique_chain_dir();
    let mut seed =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    seed.append_thought(
        "astro",
        ThoughtInput::new(ThoughtType::Summary, "seed thought"),
    )
    .unwrap();
    drop(seed);

    let mut live =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    live.set_auto_flush(true).unwrap();
    let storage_path = PathBuf::from(live.storage_location());
    let sidecar_path = dir.join(
        chain_storage_filename("source", StorageAdapterKind::Binary)
            .trim_end_matches(".tcbin")
            .to_string()
            + ".agents.json",
    );
    let live = Arc::new(RwLock::new(live));
    let survivor = Arc::clone(&live);

    let state = dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: true,
    };
    state.chains.insert("source".to_string(), live);
    let router = dashboard_impl::dashboard_router(state.clone());

    let deleted = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), axum::http::StatusCode::OK);
    assert!(state.chains.get("source").is_none());
    assert!(!storage_path.exists());
    assert!(!sidecar_path.exists());
    assert!(!load_registered_chains(&dir)
        .unwrap()
        .chains
        .contains_key("source"));

    drop(survivor);

    assert!(!load_registered_chains(&dir)
        .unwrap()
        .chains
        .contains_key("source"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_skips_deleted_cached_chains_after_external_removal() {
    let dir = unique_chain_dir();
    let mut seed =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    seed.append_thought(
        "astro",
        ThoughtInput::new(ThoughtType::Summary, "seed thought"),
    )
    .unwrap();
    drop(seed);

    let live = Arc::new(RwLock::new(
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap(),
    ));
    let state = dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(&dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: None,
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: true,
    };
    state.chains.insert("source".to_string(), Arc::clone(&live));
    let router = dashboard_impl::dashboard_router(state.clone());

    deregister_chain(&dir, "source").unwrap();

    let chains = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chains.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(chains.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 0);
    assert!(state.chains.get("source").is_none());

    let thoughts = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/thoughts?page=1&per_page=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(thoughts.status(), axum::http::StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn deleted_chain_stale_read_does_not_recreate_it() {
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

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), axum::http::StatusCode::OK);

    let stale_read = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/thoughts?per_page=10&page=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stale_read.status(), axum::http::StatusCode::NOT_FOUND);

    let chains = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chains.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(chains.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert!(json
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["chain_key"] != "source"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_agents_all_includes_live_cached_chains() {
    let dir = unique_chain_dir();
    std::fs::create_dir_all(&dir).unwrap();

    let mut live_chain = MentisDb::open_with_storage(Box::new(
        BinaryStorageAdapter::for_chain_key(&dir, "live-only"),
    ))
    .unwrap();
    live_chain
        .upsert_agent(
            "astro",
            Some("Astro"),
            Some("@gubatron"),
            Some("Live-only cached agent."),
            Some(AgentStatus::Active),
        )
        .unwrap();
    live_chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "cached only"),
        )
        .unwrap();

    let chains = Arc::new(DashMap::new());
    chains.insert("live-only".to_string(), Arc::new(RwLock::new(live_chain)));
    let router = dashboard_router_with_chains(&dir, chains);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/agents")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&body).unwrap();
    let live_entry = &json["live-only"];
    assert_eq!(live_entry["chain_key"], "live-only");
    assert_eq!(live_entry["total_agents"], 1);
    assert_eq!(live_entry["total_thoughts"], 1);
    let live_agents = live_entry["agents"].as_array().unwrap();
    assert_eq!(live_agents.len(), 1);
    assert_eq!(live_agents[0]["agent_id"], "astro");
    assert_eq!(live_agents[0]["thought_count"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}
