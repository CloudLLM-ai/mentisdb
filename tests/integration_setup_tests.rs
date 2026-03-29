use mentisdb::integrations::detect::{detect_integrations_with_environment, DetectionStatus};
use mentisdb::integrations::plan::{build_detected_setup_catalog, SetupAction};
use mentisdb::integrations::{integration_specs, IntegrationKind};
use mentisdb::paths::{HostPlatform, PathEnvironment};
use tempfile::TempDir;

fn spec(
    specs: &[mentisdb::integrations::IntegrationSpec],
    kind: IntegrationKind,
) -> &mentisdb::integrations::IntegrationSpec {
    specs
        .iter()
        .find(|entry| entry.integration == kind)
        .unwrap_or_else(|| panic!("missing spec for {}", kind.as_str()))
}

#[test]
fn macos_catalog_uses_expected_config_targets() {
    let home = TempDir::new().unwrap();
    let env = PathEnvironment {
        home_dir: Some(home.path().to_path_buf()),
        current_dir: Some(home.path().to_path_buf()),
        ..PathEnvironment::default()
    };

    let specs = integration_specs(HostPlatform::Macos, &env);

    assert_eq!(
        spec(&specs, IntegrationKind::Codex).config_target.path,
        home.path().join(".codex").join("config.toml")
    );
    assert_eq!(
        spec(&specs, IntegrationKind::OpenCode).config_target.path,
        home.path()
            .join(".config")
            .join("opencode")
            .join("opencode.json")
    );
    assert_eq!(
        spec(&specs, IntegrationKind::ClaudeCode).config_target.path,
        home.path().join(".claude.json")
    );
    assert_eq!(
        spec(&specs, IntegrationKind::VsCodeCopilot)
            .config_target
            .path,
        home.path()
            .join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("mcp.json")
    );
    assert_eq!(
        spec(&specs, IntegrationKind::ClaudeDesktop)
            .config_target
            .path,
        home.path()
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    );
}

#[test]
fn macos_detection_distinguishes_configured_and_installed() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(
        home.join(".codex").join("config.toml"),
        "[mcp_servers.mentisdb]\nurl = \"http://127.0.0.1:9471\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User"),
    )
    .unwrap();
    std::fs::write(
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("settings.json"),
        "{}",
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env.clone());

    assert_eq!(
        report.integration(IntegrationKind::Codex).unwrap().status,
        DetectionStatus::Configured
    );
    assert_eq!(
        report
            .integration(IntegrationKind::GeminiCli)
            .unwrap()
            .status,
        DetectionStatus::InstalledOrUsed
    );
    assert_eq!(
        report
            .integration(IntegrationKind::VsCodeCopilot)
            .unwrap()
            .status,
        DetectionStatus::InstalledOrUsed
    );
    assert_eq!(
        report
            .integration(IntegrationKind::ClaudeCode)
            .unwrap()
            .status,
        DetectionStatus::NotDetected
    );

    let plan = build_detected_setup_catalog(report);
    let codex = plan.integration(IntegrationKind::Codex).unwrap();
    assert_eq!(codex.action, SetupAction::UpdateExistingConfig);
    assert!(codex.targets[0].exists);

    let gemini = plan.integration(IntegrationKind::GeminiCli).unwrap();
    assert_eq!(gemini.action, SetupAction::CreateCanonicalConfig);
    assert_eq!(
        gemini.targets[0].path,
        home.join(".gemini").join("settings.json")
    );
    assert!(!gemini.targets[0].create_parent_dir);

    let vscode = plan.integration(IntegrationKind::VsCodeCopilot).unwrap();
    assert_eq!(vscode.action, SetupAction::CreateCanonicalConfig);
    assert_eq!(
        vscode.targets[0].path,
        home.join("Library")
            .join("Application Support")
            .join("Code")
            .join("User")
            .join("mcp.json")
    );
    assert!(!vscode.targets[0].create_parent_dir);
}

#[test]
fn claude_code_detection_requires_mcpservers_entry_in_settings_json() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    // ~/.claude directory is the detection probe; ~/.claude.json is the config target
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    // legacy mcp file in companion target is not sufficient for Configured status
    std::fs::create_dir_all(claude_dir.join("mcp")).unwrap();
    std::fs::write(
        claude_dir.join("mcp").join("mentisdb.json"),
        "{\"mcpServers\":{\"mentisdb\":{\"type\":\"http\",\"url\":\"http://127.0.0.1:9471\"}}}",
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env.clone());
    assert_eq!(
        report
            .integration(IntegrationKind::ClaudeCode)
            .unwrap()
            .status,
        DetectionStatus::InstalledOrUsed
    );

    // Writing mcpServers to the actual config target (~/.claude.json) triggers Configured
    std::fs::write(
        home.join(".claude.json"),
        "{\"mcpServers\":{\"mentisdb\":{\"type\":\"http\",\"url\":\"http://127.0.0.1:9471\"}}}",
    )
    .unwrap();
    let report = detect_integrations_with_environment(HostPlatform::Macos, env);
    assert_eq!(
        report
            .integration(IntegrationKind::ClaudeCode)
            .unwrap()
            .status,
        DetectionStatus::Configured
    );
}

#[test]
fn claude_code_detection_treats_realistic_settings_json_as_installed_not_configured() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    // ~/.claude directory is the detection probe; ~/.claude.json is the config target
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude.json"),
        r#"{
  "theme": "dark",
  "projects": {
    "/Users/tester/workspace/mentisdb": {
      "trust": "trusted"
    }
  }
}"#,
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home.clone()),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env);

    assert_eq!(
        report
            .integration(IntegrationKind::ClaudeCode)
            .unwrap()
            .status,
        DetectionStatus::InstalledOrUsed
    );

    let catalog = build_detected_setup_catalog(report);
    let claude = catalog.integration(IntegrationKind::ClaudeCode).unwrap();
    assert_eq!(claude.action, SetupAction::CreateCanonicalConfig);
    assert_eq!(claude.targets[0].path, home.join(".claude.json"));
    assert!(claude.targets[0].exists);
}

#[test]
fn copilot_detection_recognizes_current_mcp_servers_shape_as_configured() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".copilot")).unwrap();
    std::fs::write(
        home.join(".copilot").join("mcp-config.json"),
        r#"{
  "mcpServers": {
    "mentisdb": {
      "type": "http",
      "url": "http://127.0.0.1:9471",
      "headers": {},
      "tools": ["*"]
    },
    "github": {
      "type": "stdio",
      "command": "gh"
    }
  }
}"#,
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env);

    assert_eq!(
        report
            .integration(IntegrationKind::CopilotCli)
            .unwrap()
            .status,
        DetectionStatus::Configured
    );
}

#[test]
fn codex_detection_requires_exact_mentisdb_table_name() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(
        home.join(".codex").join("config.toml"),
        "[mcp_servers.mentisdb_backup]\nurl = \"http://127.0.0.1:9471\"\n",
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env);

    assert_eq!(
        report.integration(IntegrationKind::Codex).unwrap().status,
        DetectionStatus::InstalledOrUsed
    );
}

#[test]
fn codex_detection_ignores_similarly_named_toml_sections() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(
        home.join(".codex").join("config.toml"),
        "[mcp_servers.mentisdb_backup]\nurl = \"http://127.0.0.1:9471\"\n",
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Macos, env);

    assert_eq!(
        report.integration(IntegrationKind::Codex).unwrap().status,
        DetectionStatus::InstalledOrUsed
    );
}

#[test]
fn linux_and_windows_catalogs_follow_expected_roots() {
    let linux_env = PathEnvironment {
        home_dir: Some("/home/tester".into()),
        xdg_config_home: Some("/home/tester/.config".into()),
        current_dir: Some("/tmp".into()),
        ..PathEnvironment::default()
    };
    let linux_specs = integration_specs(HostPlatform::Linux, &linux_env);
    assert_eq!(
        spec(&linux_specs, IntegrationKind::OpenCode)
            .config_target
            .path,
        std::path::PathBuf::from("/home/tester/.config/opencode/opencode.json")
    );
    assert_eq!(
        spec(&linux_specs, IntegrationKind::VsCodeCopilot)
            .config_target
            .path,
        std::path::PathBuf::from("/home/tester/.config/Code/User/mcp.json")
    );

    let windows_env = PathEnvironment {
        user_profile: Some("C:/Users/tester".into()),
        app_data: Some("C:/Users/tester/AppData/Roaming".into()),
        current_dir: Some("C:/tmp".into()),
        ..PathEnvironment::default()
    };
    let windows_specs = integration_specs(HostPlatform::Windows, &windows_env);
    assert_eq!(
        spec(&windows_specs, IntegrationKind::OpenCode)
            .config_target
            .path,
        std::path::PathBuf::from("C:/Users/tester/AppData/Roaming/opencode/opencode.json")
    );
    assert_eq!(
        spec(&windows_specs, IntegrationKind::ClaudeDesktop)
            .config_target
            .path,
        std::path::PathBuf::from(
            "C:/Users/tester/AppData/Roaming/Claude/claude_desktop_config.json"
        )
    );
    assert_eq!(
        spec(&windows_specs, IntegrationKind::CopilotCli)
            .config_target
            .path,
        std::path::PathBuf::from("C:/Users/tester/.copilot/mcp-config.json")
    );
}

#[test]
fn copilot_cli_uses_xdg_config_home_when_present_on_macos() {
    let env = PathEnvironment {
        home_dir: Some("/Users/tester".into()),
        xdg_config_home: Some("/Users/tester/.config".into()),
        current_dir: Some("/tmp".into()),
        ..PathEnvironment::default()
    };
    let specs = integration_specs(HostPlatform::Macos, &env);

    assert_eq!(
        spec(&specs, IntegrationKind::CopilotCli).config_target.path,
        std::path::PathBuf::from("/Users/tester/.config/copilot/mcp-config.json")
    );
}

#[test]
fn copilot_cli_uses_xdg_config_home_when_present_on_linux() {
    let env = PathEnvironment {
        home_dir: Some("/home/tester".into()),
        xdg_config_home: Some("/home/tester/.config".into()),
        current_dir: Some("/tmp".into()),
        ..PathEnvironment::default()
    };
    let specs = integration_specs(HostPlatform::Linux, &env);

    assert_eq!(
        spec(&specs, IntegrationKind::CopilotCli).config_target.path,
        std::path::PathBuf::from("/home/tester/.config/copilot/mcp-config.json")
    );
}

#[test]
fn copilot_cli_detection_respects_xdg_config_home_path() {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    let xdg = home.join(".config").join("copilot");
    std::fs::create_dir_all(&xdg).unwrap();
    std::fs::write(
        xdg.join("mcp-config.json"),
        r#"{"mcpServers":{"mentisdb":{"type":"http","url":"http://127.0.0.1:9471","tools":["*"]}}}"#,
    )
    .unwrap();

    let env = PathEnvironment {
        home_dir: Some(home),
        xdg_config_home: Some(root.path().join("home").join(".config")),
        current_dir: Some(root.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let report = detect_integrations_with_environment(HostPlatform::Linux, env);

    assert_eq!(
        report
            .integration(IntegrationKind::CopilotCli)
            .unwrap()
            .status,
        DetectionStatus::Configured
    );
}

#[test]
fn default_mentisdb_dir_prefers_environment_override() {
    let dir = TempDir::new().unwrap();
    let override_dir = dir.path().join("override");
    let env = PathEnvironment {
        mentisdb_dir_override: Some(override_dir.clone()),
        home_dir: Some(dir.path().join("home")),
        current_dir: Some(dir.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    assert_eq!(env.default_mentisdb_dir(), override_dir);
}
