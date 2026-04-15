use mentisdb::integrations::apply::apply_setup_with_environment;
use mentisdb::integrations::plan::build_setup_plan_for_integration;
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
fn apply_setup_merges_claude_code_mcp_server_into_settings_json() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    // ~/.claude directory is the detection probe; ~/.claude.json is the config target
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let claude_json_path = home.join(".claude.json");
    let settings_before = r#"{
  "theme": "dark",
  "projects": {
    "/Users/tester/workspace/mentisdb": {
      "trust": "trusted"
    }
  }
}"#;
    std::fs::write(&claude_json_path, settings_before).unwrap();

    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::ClaudeCode,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json_path).unwrap()).unwrap();
    assert_eq!(parsed["theme"], "dark");
    assert_eq!(
        parsed["projects"]["/Users/tester/workspace/mentisdb"]["trust"],
        "trusted"
    );
    assert_eq!(parsed["mcpServers"]["mentisdb"]["type"], "http");
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["url"],
        "http://127.0.0.1:9471"
    );
    assert!(!claude_dir.join("mcp").join("mentisdb.json").exists());
}

#[test]
fn apply_setup_merges_current_copilot_mcp_servers_shape() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let copilot_dir = home.join(".copilot");
    std::fs::create_dir_all(&copilot_dir).unwrap();
    let config_path = copilot_dir.join("mcp-config.json");
    std::fs::write(
        &config_path,
        r#"{
  "mcpServers": {
    "github": {
      "type": "stdio",
      "command": "gh"
    }
  },
  "preferences": {
    "theme": "dark"
  }
}"#,
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::CopilotCli,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(parsed["preferences"]["theme"], "dark");
    assert_eq!(parsed["mcpServers"]["github"]["command"], "gh");
    assert_eq!(parsed["mcpServers"]["mentisdb"]["type"], "http");
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["url"],
        "http://127.0.0.1:9471"
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["headers"],
        serde_json::json!({})
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["tools"],
        serde_json::json!(["*"])
    );
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
fn apply_setup_writes_copilot_cli_config_under_xdg_root() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let xdg_root = home.join(".config");
    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        xdg_config_home: Some(xdg_root.clone()),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let result = apply_setup_with_environment(
        IntegrationKind::CopilotCli,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Linux,
        &env,
    )
    .unwrap();

    assert!(result.changed);
    let config_path = xdg_root.join("copilot").join("mcp-config.json");
    let parsed: Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).unwrap()).unwrap();
    assert_eq!(parsed["mcpServers"]["mentisdb"]["type"], "http");
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["url"],
        "http://127.0.0.1:9471"
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["headers"],
        serde_json::json!({})
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["tools"],
        serde_json::json!(["*"])
    );
}

#[test]
fn apply_claude_desktop_uses_stdio_mode() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let mentisdbd = bin_dir.join("mentisdbd");
    std::fs::write(&mentisdbd, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mentisdbd, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

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

    // mentisdbd is used directly as the command with --mode stdio
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["command"],
        mentisdbd.display().to_string(),
        "command should be the mentisdbd path"
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["args"][0], "--mode",
        "first arg should be --mode"
    );
    assert_eq!(
        parsed["mcpServers"]["mentisdb"]["args"][1], "stdio",
        "second arg should be stdio"
    );
    // No URL needed for stdio mode
    assert!(
        parsed["mcpServers"]["mentisdb"]["env"].is_null(),
        "env should be empty for stdio mode"
    );
}

#[test]
fn claude_desktop_plan_snippet_uses_stdio_mode() {
    let env = PathEnvironment {
        home_dir: Some("/Users/tester".into()),
        ..PathEnvironment::default()
    };
    let plan = build_setup_plan_for_integration(
        IntegrationKind::ClaudeDesktop,
        "https://my.mentisdb.com:9473",
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    let snippet = plan.snippet.as_deref().unwrap();
    assert!(
        snippet.contains("\"command\": \"mentisdbd\""),
        "snippet should use mentisdbd as command, got: {snippet}"
    );
    assert!(
        snippet.contains("\"--mode\"") && snippet.contains("\"stdio\""),
        "snippet should have --mode stdio args, got: {snippet}"
    );
    assert!(
        !snippet.contains("mcp-remote"),
        "snippet should not mention mcp-remote, got: {snippet}"
    );
    assert!(
        !plan
            .notes
            .iter()
            .any(|n| n.contains("Node.js") || n.contains("Homebrew")),
        "plan notes should not mention Node.js or Homebrew anymore, got: {:?}",
        plan.notes
    );
}
