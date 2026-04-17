#![cfg(feature = "server")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use chrono::DateTime;
use mentisdb::server::{
    adopt_legacy_default_mentisdb_dir, mcp_router, rest_router, standard_mcp_router,
    MentisDbServerConfig, MentisDbServiceConfig,
};
use mentisdb::{MentisDb, StorageAdapterKind, MENTISDB_CURRENT_VERSION};
use serde_json::json;
use tower::util::ServiceExt;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);
static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
const EMBEDDED_SKILL_MD: &str = include_str!("../MENTISDB_SKILL.md");

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("mentisdb_server_test_{}_{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn unique_log_file_path() -> PathBuf {
    unique_chain_dir().join("mentisdb-interactions.log")
}

fn env_mutex() -> &'static Mutex<()> {
    ENV_MUTEX.get_or_init(|| Mutex::new(()))
}

async fn append_thought_via_rest(
    router: axum::Router,
    chain_key: &str,
    agent_id: &str,
    thought_type: &str,
    role: Option<&str>,
    content: &str,
) -> serde_json::Value {
    let mut payload = json!({
        "chain_key": chain_key,
        "agent_id": agent_id,
        "thought_type": thought_type,
        "content": content
    });
    if let Some(role) = role {
        payload["role"] = json!(role);
    }

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn rest_router_loads_default_managed_vector_sidecar() {
    let dir = unique_chain_dir();
    let chain_key = "vector-default";
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Binary,
    ));

    append_thought_via_rest(
        router,
        chain_key,
        "astro",
        "Insight",
        None,
        "Latency budget for the rollout",
    )
    .await;

    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, chain_key, StorageAdapterKind::Binary)
            .unwrap();
    chain.apply_persisted_managed_vector_sidecars().unwrap();

    let statuses = chain.managed_vector_sidecar_statuses().unwrap();
    assert!(
        !statuses.is_empty(),
        "at least one vector sidecar should be registered"
    );
    let active = statuses
        .iter()
        .find(|s| s.enabled)
        .expect("an enabled sidecar should exist");

    let sidecar = chain
        .load_vector_sidecar(&active.metadata)
        .unwrap()
        .unwrap();
    assert_eq!(
        chain
            .vector_sidecar_freshness(&sidecar, &active.metadata)
            .unwrap(),
        mentisdb::search::VectorSidecarFreshness::Fresh
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn server_config_parses_mentisdb_verbose_env_values() {
    let _guard = env_mutex().lock().unwrap();
    let original = std::env::var("MENTISDB_VERBOSE").ok();

    for (raw_value, expected) in [
        ("1", true),
        ("0", false),
        ("true", true),
        ("false", false),
        ("TRUE", true),
        ("FALSE", false),
        ("unexpected", false),
    ] {
        std::env::set_var("MENTISDB_VERBOSE", raw_value);
        let config = MentisDbServerConfig::from_env();
        assert_eq!(
            config.service.verbose, expected,
            "raw value {raw_value:?} should parse to {expected}"
        );
    }

    std::env::remove_var("MENTISDB_VERBOSE");
    assert!(MentisDbServerConfig::from_env().service.verbose);

    if let Some(original) = original {
        std::env::set_var("MENTISDB_VERBOSE", original);
    } else {
        std::env::remove_var("MENTISDB_VERBOSE");
    }
}

#[test]
fn server_config_parses_mentisdb_log_file_env_values() {
    let _guard = env_mutex().lock().unwrap();
    let original = std::env::var("MENTISDB_LOG_FILE").ok();
    let log_path = unique_log_file_path();

    std::env::set_var("MENTISDB_LOG_FILE", &log_path);
    let config = MentisDbServerConfig::from_env();
    assert_eq!(config.service.log_file.as_deref(), Some(log_path.as_path()));

    std::env::remove_var("MENTISDB_LOG_FILE");
    assert!(MentisDbServerConfig::from_env().service.log_file.is_none());

    if let Some(original) = original {
        std::env::set_var("MENTISDB_LOG_FILE", original);
    } else {
        std::env::remove_var("MENTISDB_LOG_FILE");
    }
}

#[test]
fn legacy_default_storage_root_is_adopted_before_server_config_uses_default_dir() {
    let _guard = env_mutex().lock().unwrap();
    let original_home = std::env::var("HOME").ok();
    let original_dir = std::env::var("MENTISDB_DIR").ok();

    let home_dir = unique_chain_dir();
    let legacy_dir = home_dir.join(".cloudllm").join("thoughtchain");
    let mentisdb_dir = home_dir.join(".cloudllm").join("mentisdb");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(legacy_dir.join("thoughtchain-registry.json"), "{}").unwrap();
    std::fs::write(legacy_dir.join("chain-note.txt"), "legacy").unwrap();

    std::env::set_var("HOME", &home_dir);
    std::env::remove_var("MENTISDB_DIR");

    let report = adopt_legacy_default_mentisdb_dir()
        .unwrap()
        .expect("legacy default storage should be adopted");
    assert_eq!(report.source_dir, legacy_dir);
    assert_eq!(report.target_dir, mentisdb_dir);

    let config = MentisDbServerConfig::from_env();
    assert_eq!(config.service.chain_dir, mentisdb_dir);
    assert!(config
        .service
        .chain_dir
        .join("mentisdb-registry.json")
        .exists());
    assert!(config.service.chain_dir.join("chain-note.txt").exists());
    assert!(!legacy_dir.exists());

    if let Some(original_home) = original_home {
        std::env::set_var("HOME", original_home);
    } else {
        std::env::remove_var("HOME");
    }
    if let Some(original_dir) = original_dir {
        std::env::set_var("MENTISDB_DIR", original_dir);
    } else {
        std::env::remove_var("MENTISDB_DIR");
    }

    let _ = std::fs::remove_dir_all(&home_dir);
}

#[tokio::test]
async fn mcp_router_lists_mentisdb_tools() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/list")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let tools = json["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "mentisdb_append"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_append_retrospective"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_chains"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_agents"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_get_agent"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_agent_registry"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_upsert_agent"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_set_agent_description"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_add_agent_alias"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_add_agent_key"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_revoke_agent_key"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_disable_agent"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_skills"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_skill_manifest"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_upload_skill"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_search_skill"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_read_skill"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_skill_versions"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_deprecate_skill"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_revoke_skill"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_lexical_search"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_ranked_search"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_context_bundles"));
    assert!(tools.iter().any(|tool| tool["name"] == "mentisdb_skill_md"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_get_thought"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_get_genesis_thought"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_traverse_thoughts"));
    assert!(tools.iter().any(|tool| tool["name"] == "mentisdb_head"));

    let search_skill = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_search_skill")
        .unwrap();
    let search_parameters = search_skill["parameters"].as_array().unwrap();
    assert!(search_parameters
        .iter()
        .any(|parameter| parameter["name"] == "chain_key"));
    assert!(search_parameters
        .iter()
        .any(|parameter| parameter["name"] == "uploaded_by_agent_names"));
    assert!(search_parameters
        .iter()
        .any(|parameter| parameter["name"] == "uploaded_by_agent_owners"));

    let read_skill = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_read_skill")
        .unwrap();
    assert!(read_skill["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|parameter| parameter["name"] == "chain_key"));

    let lifecycle_tools = [
        "mentisdb_list_skills",
        "mentisdb_skill_versions",
        "mentisdb_deprecate_skill",
        "mentisdb_revoke_skill",
    ];
    for tool_name in lifecycle_tools {
        let tool = tools.iter().find(|tool| tool["name"] == tool_name).unwrap();
        assert!(tool["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|parameter| parameter["name"] == "chain_key"));
    }

    let get_thought = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_get_thought")
        .unwrap();
    let get_thought_parameters = get_thought["parameters"].as_array().unwrap();
    assert!(get_thought_parameters
        .iter()
        .any(|parameter| parameter["name"] == "thought_id"));
    assert!(get_thought_parameters
        .iter()
        .any(|parameter| parameter["name"] == "thought_hash"));
    assert!(get_thought_parameters
        .iter()
        .any(|parameter| parameter["name"] == "thought_index"));

    let traverse = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_traverse_thoughts")
        .unwrap();
    let traverse_parameters = traverse["parameters"].as_array().unwrap();
    assert!(traverse_parameters
        .iter()
        .any(|parameter| parameter["name"] == "anchor_boundary"));
    assert!(traverse_parameters
        .iter()
        .any(|parameter| parameter["name"] == "direction"));
    assert!(traverse_parameters
        .iter()
        .any(|parameter| parameter["name"] == "chunk_size"));
    assert!(traverse_parameters
        .iter()
        .any(|parameter| parameter["name"] == "time_window"));

    let ranked = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_ranked_search")
        .unwrap();
    let ranked_parameters = ranked["parameters"].as_array().unwrap();
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "graph"));
    assert!(ranked_parameters
        .iter()
        .any(|parameter| parameter["name"] == "offset"));

    let bundles = tools
        .iter()
        .find(|tool| tool["name"] == "mentisdb_context_bundles")
        .unwrap();
    let bundle_parameters = bundles["parameters"].as_array().unwrap();
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "text"));
    assert!(bundle_parameters
        .iter()
        .any(|parameter| parameter["name"] == "graph"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_execute_returns_embedded_skill_markdown() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_skill_md",
                        "parameters": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["result"]["success"], true);
    assert_eq!(json["result"]["output"]["markdown"], EMBEDDED_SKILL_MD);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_router_manages_skill_registry() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "skills-chain",
        StorageAdapterKind::Binary,
    ));
    let markdown = r#"---
schema_version: 1
name: MCP Registry Skill
description: Skill uploaded through MCP
tags: [mentisdb, mcp]
triggers: [registry]
warnings: [review-before-execution]
---

# MCP Registry Skill

Skill uploaded through MCP

## Usage

Use the MCP skill registry endpoints for reusable instructions.
"#;

    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_upsert_agent",
                        "parameters": {
                            "chain_key": "skills-chain",
                            "agent_id": "astro",
                            "display_name": "Astro",
                            "agent_owner": "@gubatron",
                            "status": "active"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_upload_skill",
                        "parameters": {
                            "chain_key": "skills-chain",
                            "agent_id": "astro",
                            "format": "markdown",
                            "content": markdown
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let upload_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(upload.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        upload_json["result"]["output"]["skill"]["skill_id"],
        "mcp-registry-skill"
    );

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_list_skills",
                        "parameters": {
                            "chain_key": "skills-chain"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(list.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        list_json["result"]["output"]["skills"][0]["skill_id"],
        "mcp-registry-skill"
    );

    let read = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_read_skill",
                        "parameters": {
                            "skill_id": "mcp-registry-skill",
                            "format": "json"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let read_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(read.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read_json["result"]["output"]["status"], "active");
    assert!(read_json["result"]["output"]["content"]
        .as_str()
        .unwrap()
        .contains("\"name\": \"MCP Registry Skill\""));
    assert!(read_json["result"]["output"]["safety_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning == "review-before-execution"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_bootstraps_and_reports_head() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));

    let health = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    let bootstrap = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/bootstrap")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "server-test",
                        "storage_adapter": "binary",
                        "content": "Bootstrap memory for the server test.",
                        "importance": 1.0
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), StatusCode::OK);

    let head = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/head")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "server-test"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let body = axum::body::to_bytes(head.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["thought_count"], 1);
    assert_eq!(json["integrity_ok"], true);

    let chains = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chains.status(), StatusCode::OK);
    let chains_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(chains.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let summary = chains_json["chains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["chain_key"] == "server-test")
        .unwrap();
    assert_eq!(summary["version"], MENTISDB_CURRENT_VERSION);
    assert_eq!(summary["storage_adapter"], "binary");
    assert_eq!(summary["thought_count"], 1);
    assert_eq!(summary["agent_count"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_writes_interaction_logs_to_file_when_console_logging_is_disabled() {
    let dir = unique_chain_dir();
    let log_path = dir.join("interactions.log");
    let router = rest_router(
        MentisDbServiceConfig::new(dir.clone(), "log-file", StorageAdapterKind::Binary)
            .with_verbose(false)
            .with_log_file(Some(log_path.clone())),
    );

    append_thought_via_rest(router, "log-file", "astro", "Insight", None, "log me").await;

    let log_contents = std::fs::read_to_string(&log_path).unwrap();
    assert!(log_contents.contains("[mentisdbd]"));
    assert!(log_contents.contains("op=append"));
    assert!(log_contents.contains("chain=log-file"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_router_gets_thought_by_id_and_hash() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "lookup-mcp",
        StorageAdapterKind::Binary,
    ));

    let appended = append_thought_via_rest(
        rest_router(MentisDbServiceConfig::new(
            dir.clone(),
            "lookup-mcp",
            StorageAdapterKind::Binary,
        )),
        "lookup-mcp",
        "astro",
        "Insight",
        None,
        "Lookup me through MCP.",
    )
    .await;
    let thought_id = appended["thought"]["id"].as_str().unwrap().to_string();
    let thought_hash = appended["thought"]["hash"].as_str().unwrap().to_string();

    for parameters in [
        json!({ "chain_key": "lookup-mcp", "thought_id": thought_id }),
        json!({ "chain_key": "lookup-mcp", "thought_hash": thought_hash }),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/tools/execute")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "tool": "mentisdb_get_thought",
                            "parameters": parameters
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            json["result"]["output"]["thought"]["content"],
            "Lookup me through MCP."
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_router_traverses_thoughts_forward_and_backward() {
    let dir = unique_chain_dir();
    let mcp = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "traverse-mcp",
        StorageAdapterKind::Binary,
    ));
    let rest = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "traverse-mcp",
        StorageAdapterKind::Binary,
    ));

    append_thought_via_rest(rest.clone(), "traverse-mcp", "astro", "Insight", None, "t0").await;
    let anchor = append_thought_via_rest(
        rest.clone(),
        "traverse-mcp",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "t1",
    )
    .await;
    append_thought_via_rest(
        rest.clone(),
        "traverse-mcp",
        "apollo",
        "Decision",
        Some("Checkpoint"),
        "t2",
    )
    .await;
    append_thought_via_rest(
        rest,
        "traverse-mcp",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "t3",
    )
    .await;

    let forward = mcp
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_traverse_thoughts",
                        "parameters": {
                            "chain_key": "traverse-mcp",
                            "anchor_id": anchor["thought"]["id"],
                            "direction": "forward",
                            "include_anchor": false,
                            "chunk_size": 2,
                            "agent_ids": ["astro"],
                            "thought_types": ["Decision"],
                            "roles": ["Checkpoint"]
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forward.status(), StatusCode::OK);
    let forward_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(forward.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let forward_thoughts = forward_json["result"]["output"]["thoughts"]
        .as_array()
        .unwrap();
    assert_eq!(forward_thoughts.len(), 1);
    assert_eq!(forward_thoughts[0]["content"], "t3");

    let backward = mcp
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_traverse_thoughts",
                        "parameters": {
                            "chain_key": "traverse-mcp",
                            "anchor_boundary": "head",
                            "direction": "backward",
                            "include_anchor": true,
                            "chunk_size": 1
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backward.status(), StatusCode::OK);
    let backward_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(backward.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        backward_json["result"]["output"]["thoughts"][0]["content"],
        "t3"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_returns_embedded_skill_markdown() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));

    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mentisdb_skill_md")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/markdown; charset=utf-8")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let markdown = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(markdown, EMBEDDED_SKILL_MD);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_gets_genesis_and_specific_thought() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "lookup-rest",
        StorageAdapterKind::Binary,
    ));

    let first = append_thought_via_rest(
        router.clone(),
        "lookup-rest",
        "astro",
        "Insight",
        None,
        "first thought",
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        "lookup-rest",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "second thought",
    )
    .await;

    let genesis = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/genesis")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "lookup-rest"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(genesis.status(), StatusCode::OK);
    let genesis_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(genesis.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(genesis_json["thought"]["content"], "first thought");

    let by_id = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thought")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "lookup-rest",
                        "thought_id": first["thought"]["id"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_id.status(), StatusCode::OK);

    let by_hash = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thought")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "lookup-rest",
                        "thought_hash": first["thought"]["hash"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_hash.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_traverses_thoughts_with_filters_and_chunk_size() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "traverse-rest",
        StorageAdapterKind::Binary,
    ));

    let first = append_thought_via_rest(
        router.clone(),
        "traverse-rest",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "first match",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let second = append_thought_via_rest(
        router.clone(),
        "traverse-rest",
        "apollo",
        "Decision",
        Some("Checkpoint"),
        "wrong agent",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    let third = append_thought_via_rest(
        router.clone(),
        "traverse-rest",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "second match",
    )
    .await;

    let start = first["thought"]["timestamp"].as_str().unwrap().to_string();
    let end = third["thought"]["timestamp"].as_str().unwrap().to_string();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/traverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-rest",
                        "anchor_index": 0,
                        "direction": "forward",
                        "include_anchor": true,
                        "chunk_size": 1,
                        "agent_ids": ["astro"],
                        "thought_types": ["Decision"],
                        "roles": ["Checkpoint"],
                        "since": start,
                        "until": end
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(json["thoughts"].as_array().unwrap().len(), 1);
    assert_eq!(json["thoughts"][0]["content"], "first match");
    assert_eq!(json["next_cursor"]["index"], 0);
    let time_window_start =
        DateTime::parse_from_rfc3339(second["thought"]["timestamp"].as_str().unwrap())
            .unwrap()
            .timestamp_millis();

    let backward = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/traverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-rest",
                        "anchor_hash": third["thought"]["hash"],
                        "direction": "backward",
                        "include_anchor": false,
                        "chunk_size": 1,
                        "agent_ids": ["astro"],
                        "thought_types": ["Decision"],
                        "roles": ["Checkpoint"],
                        "time_window": {
                            "start": time_window_start,
                            "delta": 60000,
                            "unit": "milliseconds"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backward.status(), StatusCode::OK);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_traversal_rejects_invalid_direction_or_locator_payloads() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "traverse-invalid",
        StorageAdapterKind::Binary,
    ));

    let invalid_direction = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/traverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-invalid",
                        "anchor_boundary": "genesis",
                        "direction": "sideways",
                        "chunk_size": 1
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_direction.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let conflicting_locator = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thought")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-invalid",
                        "thought_id": "00000000-0000-0000-0000-000000000000",
                        "thought_index": 1
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflicting_locator.status(), StatusCode::BAD_REQUEST);

    let zero_chunk = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/traverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-invalid",
                        "anchor_boundary": "genesis",
                        "direction": "forward",
                        "chunk_size": 0
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(zero_chunk.status(), StatusCode::BAD_REQUEST);

    let bad_time_window = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts/traverse")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "traverse-invalid",
                        "anchor_boundary": "genesis",
                        "direction": "forward",
                        "chunk_size": 1,
                        "time_window": {
                            "start": 0,
                            "delta": 1,
                            "unit": "minutes"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_time_window.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_head_still_returns_latest_thought_not_genesis() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "head-latest",
        StorageAdapterKind::Binary,
    ));

    append_thought_via_rest(
        router.clone(),
        "head-latest",
        "astro",
        "Insight",
        None,
        "genesis thought",
    )
    .await;
    append_thought_via_rest(
        router.clone(),
        "head-latest",
        "astro",
        "Decision",
        Some("Checkpoint"),
        "latest thought",
    )
    .await;

    let head = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/head")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "head-latest"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(head.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(json["latest_thought"]["content"], "latest thought");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_single_thought_lookup_returns_not_found_for_unknown_id_hash() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "missing-thought",
        StorageAdapterKind::Binary,
    ));

    let missing_id = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thought")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "missing-thought",
                        "thought_id": "00000000-0000-0000-0000-000000000000"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_id.status(), StatusCode::NOT_FOUND);

    let missing_hash = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thought")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "missing-thought",
                        "thought_hash": "missing-hash"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_hash.status(), StatusCode::NOT_FOUND);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_manages_skill_registry() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "skills-chain",
        StorageAdapterKind::Binary,
    ));
    let markdown = r#"---
schema_version: 1
name: REST Registry Skill
description: Skill uploaded through REST
tags: [mentisdb, rest]
triggers: [registry, rest]
warnings: [review-before-execution]
---

# REST Registry Skill

Skill uploaded through REST

## Expert Tricks

Use `skill_manifest` before building a search form.
"#;

    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/upsert")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "agent_id": "apollo",
                        "display_name": "Apollo",
                        "agent_owner": "@gubatron",
                        "status": "active"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "agent_id": "apollo",
                        "content": markdown
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);
    let upload_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(upload.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(upload_json["skill"]["skill_id"], "rest-registry-skill");
    let version_id = upload_json["skill"]["latest_version_id"]
        .as_str()
        .unwrap()
        .to_string();

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/skills?chain_key=skills-chain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(list.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(list_json["skills"][0]["skill_id"], "rest-registry-skill");

    let manifest = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/skills/manifest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    let manifest_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(manifest.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(manifest_json["manifest"]["searchable_fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field == "uploaded_by_agent_names"));

    let search = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/search")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "uploaded_by_agent_names": ["Apollo"],
                        "formats": ["markdown"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);
    let search_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(search.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(search_json["skills"][0]["skill_id"], "rest-registry-skill");

    let read = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/read")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "skill_id": "rest-registry-skill",
                        "version_id": version_id,
                        "format": "json"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let read_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(read.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read_json["status"], "active");
    assert!(read_json["content"]
        .as_str()
        .unwrap()
        .contains("\"name\": \"REST Registry Skill\""));
    assert!(read_json["safety_warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning == "review-before-execution"));

    let versions = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/versions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "skill_id": "rest-registry-skill"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(versions.status(), StatusCode::OK);
    let versions_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(versions.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(versions_json["versions"].as_array().unwrap().len(), 1);

    let deprecate = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/deprecate")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "skill_id": "rest-registry-skill",
                        "reason": "superseded"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deprecate.status(), StatusCode::OK);
    let deprecate_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(deprecate.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(deprecate_json["skill"]["status"], "deprecated");

    let revoke = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/revoke")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "skills-chain",
                        "skill_id": "rest-registry-skill",
                        "reason": "unsafe"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke.status(), StatusCode::OK);
    let revoke_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(revoke.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(revoke_json["skill"]["status"], "revoked");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_supports_shared_chain_agent_identity() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "shared-chain",
        StorageAdapterKind::Binary,
    ));

    let append = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-chain",
                        "agent_id": "agent-42",
                        "agent_name": "Planner",
                        "agent_owner": "ops-team",
                        "thought_type": "Decision",
                        "content": "Retry with exponential backoff."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append.status(), StatusCode::OK);

    let search = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-chain",
                        "agent_names": ["Planner"],
                        "agent_owners": ["ops-team"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);
    let body = axum::body::to_bytes(search.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let thoughts = json["thoughts"].as_array().unwrap();
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0]["agent_id"], "agent-42");
    assert_eq!(thoughts[0]["agent_name"], "Planner");
    assert_eq!(thoughts[0]["agent_owner"], "ops-team");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_searches_by_timestamp_window() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "time-window",
        StorageAdapterKind::Binary,
    ));

    let first_append = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "time-window",
                        "agent_id": "agent-1",
                        "thought_type": "Insight",
                        "content": "First timed thought."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_append.status(), StatusCode::OK);
    let first_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(first_append.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let first_timestamp = first_json["thought"]["timestamp"]
        .as_str()
        .unwrap()
        .to_string();

    tokio::time::sleep(Duration::from_millis(5)).await;

    let second_append = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "time-window",
                        "agent_id": "agent-1",
                        "thought_type": "Insight",
                        "content": "Second timed thought."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_append.status(), StatusCode::OK);
    let second_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(second_append.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let second_timestamp = second_json["thought"]["timestamp"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(first_timestamp, second_timestamp);

    let search = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "time-window",
                        "since": second_timestamp,
                        "until": second_timestamp
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);
    let search_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(search.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let thoughts = search_json["thoughts"].as_array().unwrap();
    assert_eq!(thoughts.len(), 1);
    assert_eq!(thoughts[0]["content"], "Second timed thought.");
    assert_eq!(
        thoughts[0]["timestamp"],
        second_json["thought"]["timestamp"]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_appends_retrospective_with_defaults() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "shared-chain",
        StorageAdapterKind::Binary,
    ));

    let append = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/retrospectives")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-chain",
                        "agent_id": "astro",
                        "agent_name": "Astro",
                        "content": "After a repeated tool-call failure, respond to every tool_call_id before sending the next model request."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append.status(), StatusCode::OK);
    let body = axum::body::to_bytes(append.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["thought"]["thought_type"], "LessonLearned");
    assert_eq!(json["thought"]["role"], "Retrospective");
    assert_eq!(json["thought"]["agent_name"], "Astro");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_lists_chains_and_agents() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "shared-brain",
        StorageAdapterKind::Binary,
    ));

    let append_one = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-brain",
                        "agent_id": "astro",
                        "agent_name": "Astro",
                        "thought_type": "Decision",
                        "content": "Use the shared chain for memory."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append_one.status(), StatusCode::OK);

    let append_two = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-brain",
                        "agent_id": "apollo",
                        "agent_name": "Apollo",
                        "agent_owner": "@gubatron",
                        "thought_type": "Insight",
                        "content": "Shared memory helps future agents resume."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(append_two.status(), StatusCode::OK);

    let chains = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/chains")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chains.status(), StatusCode::OK);
    let chains_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(chains.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let chain_keys = chains_json["chain_keys"].as_array().unwrap();
    assert!(chain_keys.iter().any(|value| value == "shared-brain"));
    assert_eq!(chains_json["default_chain_key"], "shared-brain");
    let summary = chains_json["chains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["chain_key"] == "shared-brain")
        .unwrap();
    assert_eq!(summary["version"], MENTISDB_CURRENT_VERSION);
    assert_eq!(summary["storage_adapter"], "binary");
    assert_eq!(summary["thought_count"], 2);
    assert_eq!(summary["agent_count"], 2);

    let agents = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "shared-brain"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(agents.status(), StatusCode::OK);
    let agents_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(agents.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let agent_entries = agents_json["agents"].as_array().unwrap();
    assert!(agent_entries
        .iter()
        .any(|agent| agent["agent_name"] == "Astro" && agent["agent_id"] == "astro"));
    assert!(agent_entries.iter().any(|agent| {
        agent["agent_name"] == "Apollo"
            && agent["agent_id"] == "apollo"
            && agent["agent_owner"] == "@gubatron"
    }));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_router_manages_agent_registry_records() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "registry-admin",
        StorageAdapterKind::Binary,
    ));

    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/upsert")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin",
                        "display_name": "Registry Admin",
                        "agent_owner": "@gubatron",
                        "description": "Admin test agent",
                        "status": "active"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let alias = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/aliases")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin",
                        "alias": "astro-admin"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alias.status(), StatusCode::OK);

    let add_key = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin",
                        "key_id": "main-ed25519",
                        "algorithm": "ed25519",
                        "public_key_bytes": [1, 2, 3, 4]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(add_key.status(), StatusCode::OK);

    let revoke_key = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/keys/revoke")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin",
                        "key_id": "main-ed25519"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoke_key.status(), StatusCode::OK);

    let disable = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/disable")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disable.status(), StatusCode::OK);

    let get_agent = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin",
                        "agent_id": "agent-admin"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_agent.status(), StatusCode::OK);
    let agent_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(get_agent.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(agent_json["agent"]["display_name"], "Registry Admin");
    assert_eq!(agent_json["agent"]["owner"], "@gubatron");
    assert_eq!(agent_json["agent"]["description"], "Admin test agent");
    assert_eq!(agent_json["agent"]["status"], "Revoked");
    assert!(agent_json["agent"]["aliases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|alias| alias == "astro-admin"));
    assert_eq!(
        agent_json["agent"]["public_keys"][0]["algorithm"],
        "Ed25519"
    );
    assert!(agent_json["agent"]["public_keys"][0]["revoked_at"].is_string());

    let registry = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent-registry")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "registry-admin"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(registry.status(), StatusCode::OK);
    let registry_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(registry.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(registry_json["agents"].as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn live_mcp_server_supports_standard_initialize_and_tools_list() {
    let dir = unique_chain_dir();
    let router = standard_mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));
    let client_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 49000));

    let mut initialize_request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "mentisdb-test",
                        "version": "0.1.0"
                    }
                }
            })
            .to_string(),
        ))
        .unwrap();
    initialize_request
        .extensions_mut()
        .insert(ConnectInfo(client_addr));
    let initialize = router.clone().oneshot(initialize_request).await.unwrap();
    assert_eq!(initialize.status(), StatusCode::OK);
    assert_eq!(
        initialize
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let initialize_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(initialize.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(initialize_json["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(initialize_json["result"]["serverInfo"]["name"], "mentisdb");
    assert_eq!(
        initialize_json["result"]["capabilities"]["resources"]["listChanged"],
        json!(false)
    );
    let instructions = initialize_json["result"]["instructions"]
        .as_str()
        .expect("initialize instructions must be present");
    assert!(instructions.contains("mentisdb://skill/core"));
    assert!(instructions.contains("mentisdb_list_chains"));
    assert!(instructions.contains("mentisdb_ranked_search"));
    assert!(instructions.contains("mentisdb_context_bundles"));

    let mut initialized_request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })
            .to_string(),
        ))
        .unwrap();
    initialized_request
        .extensions_mut()
        .insert(ConnectInfo(client_addr));
    let initialized = router.clone().oneshot(initialized_request).await.unwrap();
    assert_eq!(initialized.status(), StatusCode::ACCEPTED);

    let mut tools_list_request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .header("MCP-Protocol-Version", "2025-06-18")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list",
                "params": {}
            })
            .to_string(),
        ))
        .unwrap();
    tools_list_request
        .extensions_mut()
        .insert(ConnectInfo(client_addr));
    let tools_list = router.clone().oneshot(tools_list_request).await.unwrap();
    assert_eq!(tools_list.status(), StatusCode::OK);
    let tools_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(tools_list.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let tools = tools_json["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| tool["name"] == "mentisdb_append"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_append_retrospective"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_chains"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_list_agents"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_get_agent"));
    assert!(tools
        .iter()
        .any(|tool| tool["name"] == "mentisdb_upsert_agent"));
    assert!(tools.iter().any(|tool| tool["name"] == "mentisdb_head"));

    let mut resources_list_request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .header("MCP-Protocol-Version", "2025-06-18")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "resources/list",
                "params": {}
            })
            .to_string(),
        ))
        .unwrap();
    resources_list_request
        .extensions_mut()
        .insert(ConnectInfo(client_addr));
    let resources_list = router
        .clone()
        .oneshot(resources_list_request)
        .await
        .unwrap();
    assert_eq!(resources_list.status(), StatusCode::OK);
    let resources_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resources_list.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let resources = resources_json["result"]["resources"].as_array().unwrap();
    assert!(resources
        .iter()
        .any(|resource| resource["uri"] == "mentisdb://skill/core"));
    assert!(resources
        .iter()
        .any(|resource| resource["metadata"]["recommended_first"] == json!(true)));

    let mut resource_read_request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .header("MCP-Protocol-Version", "2025-06-18")
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "resources/read",
                "params": {
                    "uri": "mentisdb://skill/core"
                }
            })
            .to_string(),
        ))
        .unwrap();
    resource_read_request
        .extensions_mut()
        .insert(ConnectInfo(client_addr));
    let resource_read = router.clone().oneshot(resource_read_request).await.unwrap();
    assert_eq!(resource_read.status(), StatusCode::OK);
    let resource_read_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(resource_read.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        resource_read_json["result"]["contents"][0]["uri"],
        "mentisdb://skill/core"
    );
    assert_eq!(
        resource_read_json["result"]["contents"][0]["text"],
        EMBEDDED_SKILL_MD
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Test 7: rest_upload_skill_with_signature_verification
// ---------------------------------------------------------------------------

/// Verifies the full Ed25519 signature enforcement flow at the REST server level:
///
/// 1. Register an agent.
/// 2. Generate a real Ed25519 keypair from a fixed seed and register the public key.
/// 3. Upload a skill with a valid signature → expect HTTP 200.
/// 4. Upload again without any signature fields → expect HTTP 4xx (signature required).
/// 5. Upload again with an unknown signing key id → expect HTTP 4xx.
/// 6. Upload again with a tampered signature (one byte flipped) → expect HTTP 4xx.
#[tokio::test]
async fn rest_upload_skill_with_signature_verification() {
    use ed25519_dalek::{Signer, SigningKey};

    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "sig-test-chain",
        StorageAdapterKind::Binary,
    ));

    let skill_content = r#"---
schema_version: 1
name: Signed REST Skill
description: Skill uploaded with Ed25519 signature verification
tags: [security, signing]
triggers: [signing, ed25519]
---

# Signed REST Skill

This skill is cryptographically signed.

## Usage

Always verify signatures before trusting skill content.
"#;

    // --- Step 1: Register the uploading agent ---
    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/upsert")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "display_name": "Signing Agent",
                        "agent_owner": "@gubatron",
                        "status": "active"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK, "agent upsert must succeed");

    // --- Step 2: Generate a deterministic Ed25519 keypair from a fixed seed ---
    // Using a fixed 32-byte seed guarantees test determinism without requiring `rand`.
    let signing_key = SigningKey::from_bytes(&[42u8; 32]);
    let verifying_key = signing_key.verifying_key();
    let pub_key_bytes: Vec<u8> = verifying_key.as_bytes().to_vec();

    let add_key = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/keys")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "key_id": "ed25519-main",
                        "algorithm": "ed25519",
                        "public_key_bytes": pub_key_bytes
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        add_key.status(),
        StatusCode::OK,
        "adding public key must succeed"
    );

    // --- Step 3: Upload with a valid signature → expect 200 ---
    let valid_sig: Vec<u8> = signing_key
        .sign(skill_content.as_bytes())
        .to_bytes()
        .to_vec();

    let upload_ok = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "content": skill_content,
                        "signing_key_id": "ed25519-main",
                        "skill_signature": valid_sig
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status_ok = upload_ok.status();
    let body_ok = axum::body::to_bytes(upload_ok.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        status_ok,
        StatusCode::OK,
        "upload with valid signature must succeed; body: {}",
        String::from_utf8_lossy(&body_ok)
    );
    let ok_json: serde_json::Value = serde_json::from_slice(&body_ok).unwrap();
    assert_eq!(ok_json["skill"]["skill_id"], "signed-rest-skill");

    // --- Step 4: Upload without signature fields → expect 4xx ---
    let upload_no_sig = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "content": skill_content
                        // signing_key_id and skill_signature intentionally omitted
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        upload_no_sig.status().is_client_error(),
        "upload without signature must be rejected with a 4xx status; got: {}",
        upload_no_sig.status()
    );
    let no_sig_body = axum::body::to_bytes(upload_no_sig.into_body(), usize::MAX)
        .await
        .unwrap();
    let no_sig_json: serde_json::Value = serde_json::from_slice(&no_sig_body).unwrap();
    assert!(
        no_sig_json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("`signing_key_id` is required"),
        "missing-signature rejection should explain why; body: {}",
        String::from_utf8_lossy(&no_sig_body)
    );

    // --- Step 5: Upload with an unknown signing key id -> expect 4xx ---
    let upload_unknown_key = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "content": skill_content,
                        "signing_key_id": "missing-key",
                        "skill_signature": valid_sig
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let unknown_key_status = upload_unknown_key.status();
    let unknown_key_body = axum::body::to_bytes(upload_unknown_key.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        unknown_key_status.is_client_error(),
        "upload with unknown signing key must be rejected with a 4xx status; got: {} body: {}",
        unknown_key_status,
        String::from_utf8_lossy(&unknown_key_body)
    );
    let unknown_key_json: serde_json::Value = serde_json::from_slice(&unknown_key_body).unwrap();
    assert!(
        unknown_key_json["error"]
            .as_str()
            .unwrap_or_default()
            .contains("signing key 'missing-key' not found"),
        "unknown-key rejection should explain why; body: {}",
        String::from_utf8_lossy(&unknown_key_body)
    );

    // --- Step 6: Upload with a tampered signature (flip first byte) → expect 4xx ---
    let mut tampered_sig = valid_sig.clone();
    tampered_sig[0] ^= 0xFF; // Flip all bits in first byte.

    let upload_bad_sig = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "sig-test-chain",
                        "agent_id": "signing-agent",
                        "content": skill_content,
                        "signing_key_id": "ed25519-main",
                        "skill_signature": tampered_sig
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let bad_sig_status = upload_bad_sig.status();
    let bad_sig_body = axum::body::to_bytes(upload_bad_sig.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(
        bad_sig_status.is_client_error(),
        "upload with tampered signature must be rejected with a 4xx status; got: {} body: {}",
        bad_sig_status,
        String::from_utf8_lossy(&bad_sig_body)
    );
    let bad_sig_json: serde_json::Value = serde_json::from_slice(&bad_sig_body).unwrap();
    assert_eq!(
        bad_sig_json["error"],
        json!("Ed25519 signature verification failed")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_lexical_search_returns_ranked_scores() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Binary,
    ));
    let chain_key = "server-test";
    let _ = append_thought_via_rest(
        router.clone(),
        chain_key,
        "lexical-bot",
        "Decision",
        None,
        "Latency is trending upward",
    )
    .await;
    let _ = append_thought_via_rest(
        router.clone(),
        chain_key,
        "lexical-bot",
        "Decision",
        None,
        "Latency remains stable under the current workload",
    )
    .await;

    let payload = json!({
        "chain_key": chain_key,
        "text": "latency",
        "limit": 5,
        "offset": 0
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/lexical-search")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = parsed["results"].as_array().unwrap();
    assert!(!results.is_empty());
    assert_eq!(parsed["total"].as_u64(), Some(2));
    assert!(results[0]["score"].as_f64().unwrap_or(0.0) > 0.0);
    assert_eq!(results[0]["matched_terms"], json!(["latenc"]));
    assert!(results[0]["match_sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "content"));
}

#[tokio::test]
async fn rest_lexical_search_can_match_agent_registry_text() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test-registry",
        StorageAdapterKind::Binary,
    ));
    let chain_key = "server-test-registry";

    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/upsert")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": chain_key,
                        "agent_id": "planner",
                        "display_name": "Systems Planner",
                        "description": "Architect for lexical retrieval",
                        "status": "active"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let _ = append_thought_via_rest(
        router.clone(),
        chain_key,
        "planner",
        "Summary",
        None,
        "Keep ranked retrieval deterministic and rebuildable.",
    )
    .await;

    let payload = json!({
        "chain_key": chain_key,
        "text": "architect",
        "limit": 5,
        "offset": 0
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/lexical-search")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let results = parsed["results"].as_array().unwrap();
    assert_eq!(parsed["total"].as_u64(), Some(1));
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["matched_terms"], json!(["architect"]));
    assert!(results[0]["match_sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "agent_registry"));
}

#[tokio::test]
async fn rest_ranked_search_returns_graph_aware_results() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-graph-fields",
        StorageAdapterKind::Binary,
    ));
    let chain_key = "server-graph-fields";
    let _seed = append_thought_via_rest(
        router.clone(),
        chain_key,
        "graph-bot",
        "Decision",
        None,
        "Latency ranking seed for graph-aware transport.",
    )
    .await;
    let support_append = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": chain_key,
                        "agent_id": "graph-bot",
                        "thought_type": "Summary",
                        "content": "Supporting context reachable through relations.",
                        "refs": [0]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(support_append.status(), StatusCode::OK);

    let payload = json!({
        "chain_key": chain_key,
        "text": "latency ranking",
        "limit": 10,
        "offset": 0,
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/ranked-search")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["backend"], "hybrid_graph");
    assert_eq!(parsed["total"], 2);
    let results = parsed["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0]["score"]["total"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(results[0]["score"]["vector"].as_f64().unwrap_or(0.0) > 0.0);

    let supporting = results
        .iter()
        .find(|hit| hit["thought"]["content"] == "Supporting context reachable through relations.")
        .unwrap();
    assert_eq!(supporting["graph_distance"], 1);
    assert!(supporting["graph_seed_paths"].as_u64().unwrap_or(0) >= 1);
    assert!(supporting["graph_relation_kinds"].is_array());
    assert!(supporting["score"]["graph"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(supporting["score"]["relation"].as_f64().unwrap_or(0.0) > 0.0);
}

#[tokio::test]
async fn rest_context_bundles_returns_seed_anchored_groups() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-groups",
        StorageAdapterKind::Binary,
    ));
    let chain_key = "server-groups";

    let _seed_a = append_thought_via_rest(
        router.clone(),
        chain_key,
        "group-bot",
        "Decision",
        None,
        "Alpha seed for context bundling.",
    )
    .await;
    let _seed_b = append_thought_via_rest(
        router.clone(),
        chain_key,
        "group-bot",
        "Decision",
        None,
        "Beta seed for context bundling.",
    )
    .await;

    for (content, ref_index) in [
        ("Alpha support thought", 0_u64),
        ("Beta support thought", 1_u64),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/thoughts")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "chain_key": chain_key,
                            "agent_id": "group-bot",
                            "thought_type": "Summary",
                            "content": content,
                            "refs": [ref_index]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let payload = json!({
        "chain_key": chain_key,
        "text": "seed context bundling",
        "limit": 10,
        "offset": 0,
        "graph": {
            "mode": "incoming_only",
            "max_depth": 1
        }
    });
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/context-bundles")
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(parsed["total_bundles"], 2);
    assert!(parsed["consumed_hits"].as_u64().unwrap_or(0) >= 2);
    let bundles = parsed["bundles"].as_array().unwrap();
    assert_eq!(bundles.len(), 2);
    for bundle in bundles {
        assert!(bundle["seed"]["thought"].is_object());
        let support = bundle["support"].as_array().unwrap();
        assert_eq!(support.len(), 1);
        assert_eq!(support[0]["depth"], 1);
        assert!(support[0]["relation_kinds"].is_array());
    }
}

#[tokio::test]
async fn mcp_ranked_search_and_context_bundles_are_executable() {
    let dir = unique_chain_dir();
    let chain_key = "mcp-ranked";
    let rest = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Binary,
    ));
    let mcp = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        chain_key,
        StorageAdapterKind::Binary,
    ));

    let _seed = append_thought_via_rest(
        rest.clone(),
        chain_key,
        "mcp-bot",
        "Decision",
        None,
        "Seed thought for MCP ranked search.",
    )
    .await;
    let _ = rest
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/thoughts")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": chain_key,
                        "agent_id": "mcp-bot",
                        "thought_type": "Summary",
                        "content": "MCP support thought",
                        "refs": [0]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let ranked = mcp
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_ranked_search",
                        "parameters": {
                            "chain_key": chain_key,
                            "text": "seed thought",
                            "graph": {
                                "mode": "incoming_only",
                                "max_depth": 1
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ranked.status(), StatusCode::OK);
    let ranked_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(ranked.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(ranked_json["result"]["success"], true);
    assert_eq!(ranked_json["result"]["output"]["backend"], "hybrid_graph");
    assert!(
        ranked_json["result"]["output"]["results"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );

    let bundles = mcp
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tools/execute")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "tool": "mentisdb_context_bundles",
                        "parameters": {
                            "chain_key": chain_key,
                            "text": "seed thought",
                            "graph": {
                                "mode": "incoming_only",
                                "max_depth": 1
                            }
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bundles.status(), StatusCode::OK);
    let bundles_json: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(bundles.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(bundles_json["result"]["success"], true);
    assert!(
        bundles_json["result"]["output"]["total_bundles"]
            .as_u64()
            .unwrap_or(0)
            >= 1
    );
    assert!(bundles_json["result"]["output"]["bundles"]
        .as_array()
        .unwrap()
        .iter()
        .all(|bundle| bundle["support"].is_array()));
}

#[tokio::test]
async fn bootstrap_response_includes_empty_available_skills_when_registry_is_empty() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "spawn-test",
        StorageAdapterKind::Binary,
    ));

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/bootstrap")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "spawn-test",
                        "content": "Agent spawned with no registered skills."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(body["bootstrapped"], true);
    assert!(
        body["available_skills"].is_array(),
        "`available_skills` must be an array in the bootstrap response"
    );
    assert_eq!(
        body["available_skills"].as_array().unwrap().len(),
        0,
        "`available_skills` must be empty when the skill registry has no active skills"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bootstrap_response_includes_active_skills_after_skill_upload() {
    let dir = unique_chain_dir();
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "spawn-skill-test",
        StorageAdapterKind::Binary,
    ));

    let skill_markdown = r#"---
schema_version: 1
name: Spawn Test Skill
description: A skill to verify that bootstrap surfaces available skills on spawn.
tags: [spawn, test]
triggers: [agent-spawn]
---

# Spawn Test Skill

Load this skill immediately after bootstrap.
"#;

    let upsert = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/upsert")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "spawn-skill-test",
                        "agent_id": "spawn-agent",
                        "display_name": "Spawn Agent",
                        "status": "active"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upsert.status(), StatusCode::OK);

    let upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/skills/upload")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "spawn-skill-test",
                        "agent_id": "spawn-agent",
                        "format": "markdown",
                        "content": skill_markdown
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::OK);

    let bootstrap = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/bootstrap")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "chain_key": "spawn-skill-test",
                        "content": "Agent spawned with one registered skill."
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(bootstrap.status(), StatusCode::OK);
    let bootstrap_body: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(bootstrap.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    let skills = bootstrap_body["available_skills"]
        .as_array()
        .expect("`available_skills` must be an array");

    assert_eq!(
        skills.len(),
        1,
        "`available_skills` must contain the one active skill that was uploaded"
    );
    assert_eq!(
        skills[0]["skill_id"], "spawn-test-skill",
        "skill_id must match the uploaded skill"
    );
    assert_eq!(skills[0]["status"], "active");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn rest_ranked_search_annotates_chain_key_and_searches_ancestor_branches() {
    let dir = unique_chain_dir();
    let config = MentisDbServiceConfig::new(
        dir.clone(),
        "parent-for-branch-search",
        StorageAdapterKind::Binary,
    );
    let router = rest_router(config);

    let parent_response = append_thought_via_rest(
        router.clone(),
        "parent-for-branch-search",
        "agent1",
        "FactLearned",
        None,
        "Python uses indentation for blocks.",
    )
    .await;
    let parent_id = parent_response["thought"]["id"]
        .as_str()
        .unwrap()
        .parse::<uuid::Uuid>()
        .unwrap();

    let branch_response: serde_json::Value = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chains/branch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "source_chain_key": "parent-for-branch-search",
                            "branch_thought_id": parent_id,
                            "branch_chain_key": "child-branch-search"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        branch_response["branch_chain_key"].as_str(),
        Some("child-branch-search")
    );

    append_thought_via_rest(
        router.clone(),
        "child-branch-search",
        "agent1",
        "FactLearned",
        None,
        "Rust uses braces for blocks.",
    )
    .await;

    let search_response: serde_json::Value = {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/ranked-search")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "chain_key": "child-branch-search",
                            "text": "Python indentation"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    };

    let results = search_response["results"]
        .as_array()
        .expect("results array");
    assert!(
        !results.is_empty(),
        "cross-chain search should return results from parent chain"
    );

    let chain_keys: Vec<&str> = results
        .iter()
        .filter_map(|hit| hit["chain_key"].as_str())
        .collect();
    assert!(
        chain_keys.contains(&"parent-for-branch-search"),
        "cross-chain search must include results from parent chain, got chain_keys: {:?}",
        chain_keys
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Regression test for the MCP↔REST split-brain bug.
///
/// Before the fix, `start_servers` gave the MCP server and REST server
/// independent `MentisDbService` instances. Each service held its own
/// `DashMap<chain_key, Arc<RwLock<MentisDb>>>`, so an append via REST was
/// invisible to a read via MCP until the daemon restarted and both services
/// reloaded from disk.
///
/// This test brings up `start_servers` on ephemeral ports, appends a thought
/// via REST, and immediately reads the chain head through MCP. If the two
/// surfaces still carried separate services, MCP would report `thought_count=0`
/// and fail the assertion.
#[tokio::test]
async fn start_servers_shares_state_across_mcp_and_rest() {
    use std::net::SocketAddr;

    use mentisdb::server::start_servers;

    let dir = unique_chain_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let tls_dir = dir.join("tls");

    let service =
        MentisDbServiceConfig::new(dir.clone(), "coherency-probe", StorageAdapterKind::Binary);

    let config = MentisDbServerConfig {
        service,
        mcp_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        rest_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        https_mcp_addr: None,
        https_rest_addr: None,
        tls_cert_path: tls_dir.join("cert.pem"),
        tls_key_path: tls_dir.join("key.pem"),
        dashboard_addr: None,
        dashboard_pin: None,
    };

    let handles = start_servers(config).await.expect("start_servers");
    let mcp_url = format!("http://{}", handles.mcp.local_addr());
    let rest_url = format!("http://{}", handles.rest.local_addr());

    // Helper: call mentisdb_head over MCP and parse the head payload.
    let head_via_mcp = |url: String| async move {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "mentisdb_head",
                "arguments": {"chain_key": "coherency-probe"}
            }
        });
        let raw = tokio::task::spawn_blocking(move || {
            ureq::post(&url)
                .set("content-type", "application/json")
                .set("accept", "application/json, text/event-stream")
                .send_string(&body.to_string())
                .expect("MCP head call")
                .into_string()
                .expect("MCP head response body")
        })
        .await
        .unwrap();
        let json_str = raw
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .map(|s| s.to_string())
            .unwrap_or(raw);
        let response: serde_json::Value =
            serde_json::from_str(&json_str).expect("MCP response JSON");
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP tool result text")
            .to_string();
        serde_json::from_str::<serde_json::Value>(&text).expect("head tool output JSON")
    };

    // 0. Pre-warm the MCP service: issue a head call *before* the REST append.
    //    This forces the MCP service to open the chain into its own in-memory
    //    DashMap while the chain is empty. If MCP and REST do not share a
    //    service, REST will write the append to its own in-memory snapshot only,
    //    and the subsequent MCP head call will still see the empty pre-warmed
    //    snapshot — exposing the split-brain.
    let head_before = head_via_mcp(mcp_url.clone()).await;
    assert_eq!(
        head_before["thought_count"].as_u64().unwrap_or_default(),
        0,
        "pre-warm MCP head must show empty chain"
    );

    // 1. Append via REST.
    let append_body = json!({
        "chain_key": "coherency-probe",
        "agent_id": "probe",
        "thought_type": "FactLearned",
        "content": "coherency sentinel content"
    });
    let append_rest_url = format!("{rest_url}/v1/thoughts");
    let appended: serde_json::Value = tokio::task::spawn_blocking(move || {
        ureq::post(&append_rest_url)
            .set("content-type", "application/json")
            .send_string(&append_body.to_string())
            .expect("REST append")
            .into_json::<serde_json::Value>()
            .expect("REST append body")
    })
    .await
    .unwrap();
    let appended_index = appended["thought"]["index"]
        .as_u64()
        .expect("index in REST append response");
    let appended_head = appended["head_hash"]
        .as_str()
        .expect("head_hash in REST append response")
        .to_string();

    // 2. Read head via MCP (same surface as step 0, now after the REST append).
    let head_after = head_via_mcp(mcp_url.clone()).await;
    let mcp_thought_count = head_after["thought_count"]
        .as_u64()
        .expect("thought_count in MCP head");
    let mcp_head_hash = head_after["head_hash"]
        .as_str()
        .expect("head_hash in MCP head")
        .to_string();
    let mcp_latest_index = head_after["latest_thought"]["index"]
        .as_u64()
        .expect("latest_thought.index in MCP head");

    assert_eq!(
        mcp_thought_count,
        appended_index + 1,
        "MCP must see the REST append: expected thought_count={} got {}",
        appended_index + 1,
        mcp_thought_count
    );
    assert_eq!(
        mcp_latest_index, appended_index,
        "MCP latest index must match REST-appended index"
    );
    assert_eq!(
        mcp_head_hash, appended_head,
        "MCP head_hash must match REST head_hash"
    );

    drop(handles);
    let _ = std::fs::remove_dir_all(&dir);
}
