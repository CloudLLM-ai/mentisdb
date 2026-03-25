use mentisdb::integrations::apply::apply_setup_with_environment;
use mentisdb::integrations::IntegrationKind;
use mentisdb::paths::{HostPlatform, PathEnvironment};
use serde_json::Value;
use std::sync::{Mutex, OnceLock};
use tempfile::tempdir;
use toml_edit::DocumentMut;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

#[test]
fn apply_setup_preserves_existing_codex_toml_keys() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    std::fs::write(&config_path, "model = \"gpt-5.4\"\n").unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::Codex,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let rendered = std::fs::read_to_string(config_path).unwrap();
    let parsed = rendered.parse::<DocumentMut>().unwrap();
    assert_eq!(parsed["model"].as_str(), Some("gpt-5.4"));
    assert_eq!(
        parsed["mcp_servers"]["mentisdb"]["url"].as_str(),
        Some("http://127.0.0.1:9471")
    );
}

#[test]
fn apply_setup_supports_jsonc_and_preserves_existing_keys() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let opencode_dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    let config_path = opencode_dir.join("opencode.json");
    std::fs::write(
        &config_path,
        r#"{
  // keep comments parseable
  "agent": {}
}
"#,
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::OpenCode,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(parsed["agent"], serde_json::json!({}));
    assert_eq!(parsed["mcp"]["mentisdb"]["type"], "remote");
    assert_eq!(parsed["mcp"]["mentisdb"]["url"], "http://127.0.0.1:9471");
}

#[test]
fn apply_setup_respects_https_url_override_for_non_claude_desktop() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::OpenCode,
        "https://my.mentisdb.com:9473".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let config_path = home.join(".config").join("opencode").join("opencode.json");
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(
        parsed["mcp"]["mentisdb"]["url"],
        "https://my.mentisdb.com:9473"
    );
}

#[test]
fn apply_setup_is_idempotent_for_qwen() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let first = apply_setup_with_environment(
        IntegrationKind::Qwen,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();
    let second = apply_setup_with_environment(
        IntegrationKind::Qwen,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(first.changed);
    assert!(!second.changed);
}

#[test]
fn apply_claude_desktop_uses_https_and_bridge_from_path() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let mcp_remote = bin_dir.join("mcp-remote");
    std::fs::write(&mcp_remote, "#!/bin/sh\n").unwrap();

    let previous_path = std::env::var_os("PATH");
    std::env::set_var("PATH", &bin_dir);

    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::ClaudeDesktop,
        "https://my.mentisdb.com:9473".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    match previous_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }

    assert!(result.changed);
    let config_path = home
        .join("Library")
        .join("Application Support")
        .join("Claude")
        .join("claude_desktop_config.json");
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["command"],
        mcp_remote.display().to_string()
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["args"][0],
        "https://my.mentisdb.com:9473"
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["env"]["NODE_TLS_REJECT_UNAUTHORIZED"],
        "0"
    );
}
