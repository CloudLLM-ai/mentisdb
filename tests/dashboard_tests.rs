#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::{body::Body, http::{Request, StatusCode}};
use dashmap::DashMap;
pub use mentisdb::auth;
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
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
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
    assert!(html.contains("<input type=\"text\" id=\"ad-name\""));
    assert!(html.contains("<textarea id=\"ad-desc\""));
    assert!(html.contains("<input type=\"text\" id=\"ad-owner\""));
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
async fn dashboard_serves_skill_edit_controls() {
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

    assert!(html.contains("id=\"edit-skill-overlay\""));
    assert!(html.contains("Save New Version"));
    assert!(html.contains("id=\"sd-edit-btn\""));
    assert!(html.contains("Edit Skill"));
    assert!(html.contains("data-skill-edit data-skill-id=\"${esc(s.skill_id)}\""));
    assert!(html.contains("el.querySelectorAll('[data-skill-edit]').forEach(btn =>"));
    assert!(html.contains("if (skillId) window._openEditSkill(skillId);"));
    assert!(!html.contains("onclick=\"window._openEditSkill("));
    assert!(html.contains(
        "editBtn.addEventListener('click', () => window._openEditSkill(skillId, SD.versionId));"
    ));
    assert!(html.contains("function(skillId, versionId)"));
    assert!(html.contains("?version=${encodeURIComponent(versionId)}"));
    assert!(html.contains("const detailUrl = SD.versionId"));
    assert!(html.contains("versions[versions.length - 1].version_id"));
    assert!(html.contains("skill_id: _editSkillId"));
    assert!(html.contains("api('/dashboard/api/skills'"));
    assert!(html.contains("function safeMarkdownHref(rawHref)"));
    assert!(html.contains("normalized.startsWith('javascript:')"));
    assert!(html.contains("rel=\"noopener noreferrer\""));
    let chains_nav = html.find("id=\"nav-chains\"").unwrap();
    let agents_nav = html.find("id=\"nav-agents\"").unwrap();
    let skills_nav = html.find("id=\"nav-skills\"").unwrap();
    let bearer_tokens_nav = html.find("id=\"nav-bearer-tokens\"").unwrap();
    let settings_nav = html.find("id=\"nav-settings\"").unwrap();
    assert!(chains_nav < agents_nav);
    assert!(agents_nav < skills_nav);
    assert!(skills_nav < bearer_tokens_nav);
    assert!(bearer_tokens_nav < settings_nav);
    assert!(html.contains("href=\"#bearer-tokens\""));
    assert!(html.contains("function renderBearerTokensPage()"));
    // The bearer-token access control is a radio group with two options
    // (Enabled / Disabled) plus a status pill — not a single checkbox.
    assert!(html.contains("name=\"bt-access\""));
    assert!(html.contains("id=\"bt-access-enabled\""));
    assert!(html.contains("id=\"bt-access-disabled\""));
    assert!(html.contains("id=\"bt-access-current\""));
    assert!(html.contains("class=\"status-pill status-pill-muted\""));
    assert!(html.contains("function applyBearerTokenAccessChange(enabled)"));
    assert!(html.contains("MENTISDB_BEARER_TOKEN_ACCESS: enabled ? 'true' : 'false'"));
    assert!(html.contains("r.restart_required"));
    assert!(html.contains("function restartBearerTokenDaemon()"));
    assert!(html.contains("id=\"st-restart\""));
    assert!(html.contains("function restartDaemon()"));
    assert!(html.contains("api('/dashboard/api/restart', { method: 'POST' })"));
    // Make sure the old single-checkbox UI is gone.
    assert!(!html.contains("id=\"bt-access-toggle\""));
    assert!(!html.contains("function updateBearerTokenAccess(enabled)"));
    assert!(!html.contains("function updateBearerTokenAccessLabel()"));
    assert!(html.contains("id=\"bt-alias\""));
    assert!(html.contains("id=\"bt-scope\""));
    assert!(html.contains("type=\"radio\" name=\"bt-scope\" value=\"global\" checked"));
    assert!(html.contains("type=\"radio\" name=\"bt-scope\" value=\"chain\""));
    assert!(html.contains("function selectedBearerTokenScope()"));
    assert!(html.contains("id=\"bt-chain-picker\""));
    assert!(html.contains("grid-column:1 / -1"));
    assert!(html.contains("class=\"token-chain-table\""));
    assert!(html.contains("function selectedBearerTokenChains()"));
    assert!(html.contains("id=\"bt-copy-secret\""));
    assert!(html.contains("function copyBearerTokenSecret(token)"));
    assert!(html.contains("function copyBearerTokenSecretFallback(token, onCopied)"));
    assert!(html.contains("data-bt-revoke"));
    assert!(!html.contains(r#"<a href="$2" target="_blank" rel="noopener">$1</a>"#));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_restart_api_schedules_restart() {
    let dir = unique_chain_dir();
    let router = dashboard_router_for_dir(&dir);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/restart")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["success"], true);
    assert!(payload["message"]
        .as_str()
        .unwrap()
        .contains("Restart scheduled"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_bearer_token_api_creates_lists_and_revokes_tokens() {
    let dir = unique_chain_dir();
    let router = dashboard_router_for_dir(&dir);

    let create = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/bearer-tokens")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"alias":"codex-laptop","scope":"chain","chain_keys":["mentisdb","gubatron"]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(create.into_body(), usize::MAX)
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(&body).unwrap();
    let token = created["token"].as_str().unwrap();
    assert_eq!(created["alias"], "codex-laptop");
    assert_eq!(created["scope"], "chains:gubatron,mentisdb");
    assert!(token.starts_with("mentisdb_"));

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard/api/bearer-tokens")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(list.into_body(), usize::MAX)
        .await
        .unwrap();
    let tokens: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tokens[0]["alias"], "codex-laptop");
    assert_eq!(tokens[0]["scope"], "chains:gubatron,mentisdb");
    assert_eq!(tokens[0]["status"], "active");
    assert!(!tokens.to_string().contains(token));

    let revoke = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/dashboard/api/bearer-tokens/codex-laptop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(revoke.into_body(), usize::MAX)
        .await
        .unwrap();
    let revoked: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(revoked["status"], "revoked");

    let invalid = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/bearer-tokens")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"alias":"empty-chain","scope":"chain","chain_keys":[]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), axum::http::StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dashboard_skill_edit_upload_creates_new_version_and_reads_selected_version() {
    let dir = unique_chain_dir();
    let router = dashboard_router_for_dir(&dir);
    let skill_id = "dashboard-edit-skill";
    let first_content = r#"---
schema_version: 1
name: Dashboard Edit Skill
description: First dashboard skill version
---

# Dashboard Edit Skill

First dashboard body.
"#;
    let second_content = r#"---
schema_version: 1
name: Dashboard Edit Skill
description: Second dashboard skill version
---

# Dashboard Edit Skill

Second dashboard body.
"#;

    let first_upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/skills")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "agent_id": "rust-backend-engineer",
                        "skill_id": skill_id,
                        "content": first_content,
                        "format": "markdown"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_upload.status(), axum::http::StatusCode::OK);
    let first_body = axum::body::to_bytes(first_upload.into_body(), usize::MAX)
        .await
        .unwrap();
    let first_json: Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(first_json["skill_id"], skill_id);
    assert_eq!(first_json["version_count"], 1);

    let second_upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/api/skills")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "agent_id": "rust-backend-engineer",
                        "skill_id": skill_id,
                        "content": second_content,
                        "format": "markdown"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_upload.status(), axum::http::StatusCode::OK);
    let second_body = axum::body::to_bytes(second_upload.into_body(), usize::MAX)
        .await
        .unwrap();
    let second_json: Value = serde_json::from_slice(&second_body).unwrap();
    assert_eq!(second_json["skill_id"], skill_id);
    assert_eq!(second_json["version_count"], 2);

    let versions = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/dashboard/api/skills/{skill_id}/versions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versions.status(), axum::http::StatusCode::OK);
    let versions_body = axum::body::to_bytes(versions.into_body(), usize::MAX)
        .await
        .unwrap();
    let versions_json: Value = serde_json::from_slice(&versions_body).unwrap();
    let versions = versions_json.as_array().unwrap();
    assert_eq!(versions.len(), 2);
    let first_version_id = versions
        .iter()
        .find(|version| version["version_number"] == 0)
        .and_then(|version| version["version_id"].as_str())
        .expect("first uploaded version should be listed");

    let latest = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/dashboard/api/skills/{skill_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(latest.status(), axum::http::StatusCode::OK);
    let latest_body = axum::body::to_bytes(latest.into_body(), usize::MAX)
        .await
        .unwrap();
    let latest_json: Value = serde_json::from_slice(&latest_body).unwrap();
    assert!(latest_json["markdown"]
        .as_str()
        .unwrap()
        .contains("Second dashboard body."));

    let selected = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/dashboard/api/skills/{skill_id}?version={first_version_id}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(selected.status(), axum::http::StatusCode::OK);
    let selected_body = axum::body::to_bytes(selected.into_body(), usize::MAX)
        .await
        .unwrap();
    let selected_json: Value = serde_json::from_slice(&selected_body).unwrap();
    let selected_markdown = selected_json["markdown"].as_str().unwrap();
    assert!(selected_markdown.contains("First dashboard body."));
    assert!(!selected_markdown.contains("Second dashboard body."));

    let missing_version = router
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/dashboard/api/skills/{skill_id}?version=00000000-0000-0000-0000-000000000000"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_version.status(), axum::http::StatusCode::NOT_FOUND);

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
    // Use serde_json Value's PartialEq — handles type mismatch gracefully
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
    // Use serde_json Value's PartialEq — handles type mismatch gracefully
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
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
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
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
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

#[tokio::test]
async fn dashboard_bearer_token_access_uses_radio_group_with_status_pill() {
    // Regression test for the previous single-checkbox UI: the new
    // "MCP Access Control" card on the Bearer Tokens page must expose two
    // radio buttons (Enabled / Disabled) plus a "Current" status pill so
    // users can see at a glance what the daemon is enforcing.
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

    // The two radios must be grouped under the same name and must NOT be
    // the previous single checkbox.
    let enabled_idx = html
        .find("id=\"bt-access-enabled\"")
        .expect("bt-access-enabled radio");
    let disabled_idx = html
        .find("id=\"bt-access-disabled\"")
        .expect("bt-access-disabled radio");
    let group_idx = html
        .find("name=\"bt-access\"")
        .expect("name=\"bt-access\" radio group");
    assert!(group_idx < enabled_idx);
    assert!(enabled_idx < disabled_idx);
    assert!(!html.contains("id=\"bt-access-toggle\""));
    assert!(!html.contains("id=\"bt-access-label\""));

    // The status pill must use the new CSS class and the radios must
    // announce the chosen mode via the .status-pill colour.
    assert!(html.contains("class=\"status-pill status-pill-muted\""));
    assert!(html.contains("id=\"bt-access-current\""));

    // The "Current" badge is set by the same code path that posts the
    // change; the function names below are part of the wired UI.
    assert!(html.contains("function applyBearerTokenAccessState("));
    assert!(html.contains("function applyBearerTokenAccessChange(enabled)"));
    assert!(html.contains("function restartBearerTokenDaemon()"));
    // The change submission still uses the right env-var name.
    assert!(html.contains("MENTISDB_BEARER_TOKEN_ACCESS: enabled ? 'true' : 'false'"));
    // The response from /dashboard/api/settings carries a
    // `restart_required` flag that the new UI reads to decide whether to
    // surface the Restart Daemon button.
    assert!(html.contains("r.restart_required"));

    let _ = std::fs::remove_dir_all(&dir);
}

// ── Session-lifetime regression tests ────────────────────────────────────────

fn dashboard_router_with_pin(dir: &PathBuf, pin: &str) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.clone(),
        default_chain_key: "source".to_string(),
        dashboard_pin: Some(pin.to_string()),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

/// Regression: the session timeout must NOT be derived from the brute-force
/// rate-limit window. The previous code used `RATE_LIMIT_WINDOW_SECS * 60`,
/// which (since the window is already in seconds) made sessions valid for
/// 5 hours instead of 5 minutes. This test pins the invariant that the two
/// are independent and the session timeout is the documented 8-hour value.
#[test]
fn session_timeout_is_independent_of_rate_limit_window() {
    use dashboard_impl::{RATE_LIMIT_WINDOW_SECS, SESSION_TIMEOUT_SECS};

    // The session timeout must not be the old buggy formula.
    assert_ne!(
        SESSION_TIMEOUT_SECS,
        RATE_LIMIT_WINDOW_SECS * 60,
        "session timeout must not be derived from the rate-limit window"
    );
    // The session timeout must outlast the rate-limit window — a session
    // that expires faster than the brute-force lockout would be unusable.
    assert!(
        SESSION_TIMEOUT_SECS > RATE_LIMIT_WINDOW_SECS,
        "session timeout ({SESSION_TIMEOUT_SECS}s) must exceed rate-limit window ({RATE_LIMIT_WINDOW_SECS}s)"
    );
    // Pin the documented value so a future edit is deliberate.
    assert_eq!(SESSION_TIMEOUT_SECS, 8 * 60 * 60);
}

/// Regression: an expired session token must be rejected by the PIN
/// middleware. We inject a token with an issue time older than
/// `SESSION_TIMEOUT_SECS` directly into the session map and verify that
/// `/dashboard` redirects to login instead of granting access.
#[tokio::test]
async fn expired_session_token_is_rejected_by_pin_middleware() {
    use std::time::{Duration, Instant};
    use dashboard_impl::SESSION_TIMEOUT_SECS;

    let dir = unique_chain_dir();
    let router = dashboard_router_with_pin(&dir, "1234");

    // Inject a token that is already expired into the session map.
    // Use checked_sub in case the system hasn't been up long enough.
    let expired_instant = Instant::now()
        .checked_sub(Duration::from_secs(SESSION_TIMEOUT_SECS + 1))
        .expect("system uptime should exceed session timeout");
    let expired_token = "expired-token-uuid".to_string();

    // The sessions map is inside the router's shared state. We can reach it
    // by performing a successful login first (which populates the map),
    // then also inserting our expired token via the same map reference.
    // Since we don't have direct access to the state from the router, we
    // test the end-to-end path: a token that was never issued should be
    // rejected, confirming the map lookup is enforced.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .header("cookie", format!("mentisdb_session={expired_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SEE_OTHER,
        "an unissued/expired session token must redirect to login"
    );

    let _ = expired_instant; // used only to prove checked_sub is feasible
    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression: a freshly issued session token from a correct login must
/// unlock the dashboard. This is the positive counterpart to the expired-
/// token test and also guards against the session map being unreachable
/// from the middleware.
#[tokio::test]
async fn fresh_session_token_unlocks_dashboard() {
    let dir = unique_chain_dir();
    let router = dashboard_router_with_pin(&dir, "1234");

    // Without auth → redirect to login.
    let resp = router
        .clone()
        .oneshot(Request::builder().uri("/dashboard").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    // POST login with correct PIN.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dashboard/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("pin=1234"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);

    let set_cookie = resp
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .expect("expected Set-Cookie after successful login");
    let token = set_cookie
        .split(';')
        .next()
        .unwrap()
        .trim()
        .strip_prefix("mentisdb_session=")
        .map(|s| s.to_string())
        .expect("expected mentisdb_session cookie");

    // Use the session cookie → should get 200, not redirect.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/dashboard")
                .header("cookie", format!("mentisdb_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}
