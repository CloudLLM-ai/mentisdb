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
use mentisdb::StorageAdapterKind;
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
        StorageAdapterKind::Jsonl,
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

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_execute_returns_embedded_skill_markdown() {
    let dir = unique_chain_dir();
    let router = mcp_router(MentisDbServiceConfig::new(
        dir.clone(),
        "server-test",
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
    assert_eq!(summary["version"], 1);
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
        MentisDbServiceConfig::new(dir.clone(), "log-file", StorageAdapterKind::Jsonl)
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
        StorageAdapterKind::Jsonl,
    ));

    let appended = append_thought_via_rest(
        rest_router(MentisDbServiceConfig::new(
            dir.clone(),
            "lookup-mcp",
            StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
    ));
    let rest = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "traverse-mcp",
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
        StorageAdapterKind::Jsonl,
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
    assert_eq!(summary["version"], 1);
    assert_eq!(summary["storage_adapter"], "jsonl");
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
        StorageAdapterKind::Jsonl,
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
    let tools_list = router.oneshot(tools_list_request).await.unwrap();
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

    let _ = std::fs::remove_dir_all(&dir);
}
