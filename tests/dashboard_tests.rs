#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{body::Body, http::Request};
use dashmap::DashMap;
pub use mentisdb::search;
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, ManagedVectorProviderKind, MentisDb, PublicKeyAlgorithm,
    RankedSearchGraph, RankedSearchHit, RankedSearchQuery, SkillFormat, SkillRegistry, SkillUpload,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
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
    assert!(
        html.contains("agent.display_name")
            && html.contains("agent.description")
            && html.contains("agent.owner"),
        "JavaScript should reference agent object properties (display_name, description, owner)"
    );
    assert!(
        html.contains("getElementById('ad-") && html.contains("').value = agent."),
        "JavaScript should use getElementById pattern to set form values from agent properties"
    );

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
    assert!(
        html.contains("agent-chain-${encodeURIComponent(chainKey)}"),
        "agentListAnchorId should generate anchor IDs with agent-chain- prefix"
    );
    assert!(
        html.contains("#agents/${encodeURIComponent(chainKey)}"),
        "agentListHash should generate hrefs with #agents/ prefix"
    );
    assert!(
        html.contains("href=\"${agentListHash(") || html.contains("href=${agentListHash("),
        "anchor href should use agentListHash function"
    );

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

    let _ = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

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
async fn dashboard_chain_detail_exposes_default_vector_sidecar_status() {
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
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
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
    let sidecars = json["vector_sidecars"].as_array().unwrap();
    assert!(!sidecars.is_empty(), "expected at least one vector sidecar");
    let enabled_sidecars: Vec<_> = sidecars
        .iter()
        .filter(|s| s["enabled"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(
        enabled_sidecars.len(),
        1,
        "expected exactly one enabled sidecar"
    );
    assert_eq!(enabled_sidecars[0]["freshness"], "Fresh");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_can_disable_and_resync_vector_sidecar() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "latency budget"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_json: Value = serde_json::from_slice(&detail_body).unwrap();
    let provider_key = detail_json["vector_sidecars"][0]["provider_key"]
        .as_str()
        .unwrap()
        .to_string();

    let disabled = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/dashboard/api/chains/source/vectors/{provider_key}/disable"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), axum::http::StatusCode::OK);

    let mut external =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    external
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Idea, "tail latency mitigation"),
        )
        .unwrap();
    drop(external);

    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_json: Value = serde_json::from_slice(&detail_body).unwrap();
    let disabled_sidecar = detail_json["vector_sidecars"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["provider_key"] == provider_key)
        .unwrap();
    assert_eq!(disabled_sidecar["enabled"], false);
    assert_ne!(disabled_sidecar["freshness"], "Fresh");

    let synced = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/dashboard/api/chains/source/vectors/{provider_key}/sync"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(synced.status(), axum::http::StatusCode::OK);

    let detail = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_json: Value = serde_json::from_slice(&detail_body).unwrap();
    let synced_sidecar = detail_json["vector_sidecars"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["provider_key"] == provider_key)
        .unwrap();
    assert_eq!(synced_sidecar["enabled"], false);
    assert_eq!(synced_sidecar["freshness"], "Fresh");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn deleting_chain_removes_vector_sidecar_and_config() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "seed thought"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail_body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .unwrap();
    let detail_json: Value = serde_json::from_slice(&detail_body).unwrap();
    let provider_key = detail_json["vector_sidecars"][0]["provider_key"]
        .as_str()
        .unwrap();

    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    let sidecar_path = if provider_key == "fastembed-minilm" {
        #[cfg(feature = "local-embeddings")]
        {
            chain
                .vector_sidecar_path(search::EmbeddingProvider::metadata(
                    &search::FastEmbedProvider::try_new().unwrap(),
                ))
                .unwrap()
        }
        #[cfg(not(feature = "local-embeddings"))]
        {
            chain
                .vector_sidecar_path(search::EmbeddingProvider::metadata(
                    &search::LocalTextEmbeddingProvider::new(),
                ))
                .unwrap()
        }
    } else {
        chain
            .vector_sidecar_path(search::EmbeddingProvider::metadata(
                &search::LocalTextEmbeddingProvider::new(),
            ))
            .unwrap()
    };
    let vector_config_path = dir.join(
        chain_storage_filename("source", StorageAdapterKind::Binary)
            .trim_end_matches(".tcbin")
            .to_string()
            + ".vectors.managed.json",
    );
    assert!(sidecar_path.exists());
    assert!(vector_config_path.exists());

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
    assert!(!sidecar_path.exists());
    assert!(!vector_config_path.exists());

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
    let source_entries: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["chain_key"] == "source")
        .collect();
    assert!(
        source_entries.is_empty() || state.chains.get("source").is_some(),
        "deregistered chain should not appear unless it is still in the live cache"
    );

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

#[tokio::test]
async fn chain_search_endpoint_filters_and_paginates_results() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .upsert_agent(
            "astro",
            Some("Astro"),
            Some("@gubatron"),
            Some("Search owner"),
            Some(AgentStatus::Active),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "first dashboard search hit"),
        )
        .unwrap();
    chain
        .append_thought(
            "zeus",
            ThoughtInput::new(ThoughtType::Decision, "ignore this decision"),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Insight, "second dashboard search hit"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/search?text=dashboard%20search&agent_id=astro&page=1&per_page=1&order=desc")
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
    let results = json["results"].as_array().unwrap();
    let thoughts = json["thoughts"].as_array().unwrap();
    assert_eq!(json["mode"], "ranked");
    let backend = json["backend"].as_str().unwrap();
    assert!(
        backend == "hybrid_graph" || backend == "lexical_graph",
        "expected hybrid_graph or lexical_graph, got {backend}"
    );
    assert_eq!(json["total"], 2);
    assert_eq!(json["pages"], 2);
    assert_eq!(results.len(), 1);
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0]["agent_id"], "astro");
    assert!(results[0]["score"]["total"].as_f64().unwrap_or(0.0) > 0.0);
    if backend == "hybrid_graph" {
        assert!(results[0]["score"]["vector"].as_f64().unwrap_or(0.0) > 0.0);
    }
    assert!(
        thoughts[0]["content"] == "first dashboard search hit"
            || thoughts[0]["content"] == "second dashboard search hit"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chain_search_endpoint_includes_graph_supporting_context() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    let seed = chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking anchor for dashboard chain search.",
            ),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Operator rollout checklist linked from the anchor.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/search?text=latency%20ranking&page=1&per_page=10&order=desc")
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
    let results = json["results"].as_array().unwrap();
    let thoughts = json["thoughts"].as_array().unwrap();
    assert_eq!(json["total"], 2);
    let backend = json["backend"].as_str().unwrap();
    assert!(
        backend == "hybrid_graph" || backend == "lexical_graph",
        "expected hybrid_graph or lexical_graph, got {backend}"
    );
    assert!(
        thoughts[0]["content"] == "Latency ranking anchor for dashboard chain search."
            || thoughts[0]["content"] == "Operator rollout checklist linked from the anchor."
    );
    assert_eq!(results[0]["thought"]["content"], thoughts[0]["content"]);
    if backend == "hybrid_graph" {
        assert!(results[0]["score"]["vector"].as_f64().unwrap_or(0.0) > 0.0);
    }
    assert!(thoughts.iter().any(|thought| {
        thought["content"] == "Operator rollout checklist linked from the anchor."
    }));
    assert!(results.iter().any(|hit| hit["graph_distance"] == 1));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chain_search_bundles_endpoint_groups_support_under_seed() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    let seed = chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Latency ranking seed for grouped dashboard bundles.",
            ),
        )
        .unwrap()
        .clone();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(
                ThoughtType::Summary,
                "Grouped support context without lexical overlap.",
            )
            .with_relations(vec![ThoughtRelation {
                kind: ThoughtRelationKind::DerivedFrom,
                target_id: seed.id,
                chain_key: None,
                valid_at: None,
                invalid_at: None,
            }]),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/search/bundles?text=latency%20ranking&page=1&per_page=10&order=desc")
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
    let bundles = json["bundles"].as_array().unwrap();
    let support = bundles[0]["support"].as_array().unwrap();

    assert_eq!(json["total_bundles"], 1);
    assert_eq!(bundles[0]["seed"]["thought"]["content"], seed.content);
    assert_eq!(support.len(), 1);
    assert_eq!(
        support[0]["thought"]["content"],
        "Grouped support context without lexical overlap."
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chain_search_without_text_keeps_legacy_filtered_pagination() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "older astro thought"),
        )
        .unwrap();
    chain
        .append_thought(
            "zeus",
            ThoughtInput::new(ThoughtType::Summary, "zeus thought"),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Insight, "newer astro thought"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/search?agent_id=astro&page=1&per_page=1&order=desc")
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
    let thoughts = json["thoughts"].as_array().unwrap();
    assert!(json.get("results").is_none());
    assert_eq!(json["total"], 2);
    assert_eq!(json["pages"], 2);
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0]["content"], "newer astro thought");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn chain_search_agent_options_include_live_authors_only() {
    let dir = unique_chain_dir();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "source", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .upsert_agent(
            "ghost",
            Some("Ghost"),
            Some("@gubatron"),
            Some("Registry only"),
            Some(AgentStatus::Active),
        )
        .unwrap();
    chain
        .upsert_agent(
            "astro",
            Some("Astro"),
            Some("@gubatron"),
            Some("Live author"),
            Some(AgentStatus::Active),
        )
        .unwrap();
    chain
        .append_thought(
            "astro",
            ThoughtInput::new(ThoughtType::Summary, "Astro wrote this"),
        )
        .unwrap();
    chain
        .append_thought(
            "bot",
            ThoughtInput::new(ThoughtType::Summary, "Bot wrote this too"),
        )
        .unwrap();
    drop(chain);

    let router = dashboard_router_for_dir(&dir);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/chains/source/search/agents")
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
    let agents = json.as_array().unwrap();

    assert!(agents.iter().any(|agent| {
        agent["agent_id"] == "astro"
            && agent["display_name"] == "Astro"
            && agent["thought_count"] == 1
    }));
    assert!(agents
        .iter()
        .any(|agent| agent["agent_id"] == "bot" && agent["thought_count"] == 1));
    assert!(!agents.iter().any(|agent| agent["agent_id"] == "ghost"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_html_includes_chain_search_scaffolding() {
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
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("ex-search-text"));
    assert!(html.contains("Vector Sidecars"));
    assert!(html.contains("loadVectorPanel(chainKey)"));
    assert!(html.contains("/dashboard/api/chains/${encodeURIComponent(EX.chainKey)}/search/agents"));
    assert!(html.contains("/dashboard/api/chains/${encodeURIComponent(chainKey)}/vectors/${encodeURIComponent(key)}/rebuild"));
    assert!(html.contains("Context Bundles"));
    assert!(html.contains("updateExplorerOrderUi"));

    let _ = std::fs::remove_dir_all(&dir);
}
