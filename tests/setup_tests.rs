use mentisdb::cli::{
    parse_args, parse_node_major, render_setup_plan, run_with_io, CliCommand, SetupCommand,
};
use mentisdb::integrations::plan::build_setup_plan_for_integration;
use mentisdb::integrations::IntegrationKind;
use mentisdb::paths::{HostPlatform, PathEnvironment};
use std::io::Cursor;
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};
use tempfile::tempdir;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

#[test]
fn parse_setup_command_accepts_supported_agent_and_url_override() {
    let parsed = parse_args([
        "mentisdb",
        "setup",
        "codex",
        "--url",
        "http://127.0.0.1:9999",
    ])
    .unwrap();

    assert_eq!(
        parsed,
        CliCommand::Setup(SetupCommand {
            integrations: vec![IntegrationKind::Codex],
            url: Some("http://127.0.0.1:9999".to_string()),
            dry_run: false,
            assume_yes: false,
        })
    );
}

#[test]
fn parse_setup_help_returns_help_command() {
    let parsed = parse_args(["mentisdb", "setup", "--help"]).unwrap();
    assert_eq!(parsed, CliCommand::Help);
}

#[test]
fn parse_setup_command_keeps_per_integration_defaults_when_url_is_omitted() {
    let parsed = parse_args(["mentisdb", "setup", "claude-desktop"]).unwrap();

    assert_eq!(
        parsed,
        CliCommand::Setup(SetupCommand {
            integrations: vec![IntegrationKind::ClaudeDesktop],
            url: None,
            dry_run: false,
            assume_yes: false,
        })
    );
}

#[test]
fn parse_setup_command_accepts_yes_flag() {
    let parsed = parse_args(["mentisdb", "setup", "codex", "--yes"]).unwrap();

    assert_eq!(
        parsed,
        CliCommand::Setup(SetupCommand {
            integrations: vec![IntegrationKind::Codex],
            url: None,
            dry_run: false,
            assume_yes: true,
        })
    );
}

#[test]
fn macos_vscode_copilot_plan_uses_application_support_path() {
    let env = PathEnvironment {
        home_dir: Some("/Users/tester".into()),
        ..PathEnvironment::default()
    };
    let plan = build_setup_plan_for_integration(
        IntegrationKind::VsCodeCopilot,
        "http://127.0.0.1:9471",
        HostPlatform::Macos,
        &env,
    )
    .unwrap();

    assert_eq!(
        plan.spec.config_target.path,
        std::path::PathBuf::from("/Users/tester/Library/Application Support/Code/User/mcp.json")
    );
    assert!(plan
        .snippet
        .as_deref()
        .unwrap()
        .contains("\"type\": \"http\""));
}

#[test]
fn rendered_setup_plan_includes_status_and_action() {
    let env = PathEnvironment {
        home_dir: Some("/Users/tester".into()),
        ..PathEnvironment::default()
    };
    let plan = build_setup_plan_for_integration(
        IntegrationKind::Codex,
        "http://127.0.0.1:9471",
        HostPlatform::Macos,
        &env,
    )
    .unwrap();
    let rendered = render_setup_plan(&plan);

    assert!(rendered.contains("Status:"));
    assert!(rendered.contains("Action:"));
    assert!(rendered.contains("codex mcp add mentisdb --url http://127.0.0.1:9471"));
}

#[test]
fn help_text_lists_all_supported_agents_and_commands() {
    let help = mentisdb::cli::parse_args(["mentisdb", "--help"]);
    assert!(help.is_ok());

    let text = {
        use mentisdb::cli::run_with_io;
        use std::io::Cursor;
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let _ = run_with_io(["mentisdb", "--help"], &mut input, &mut output, &mut errors);
        String::from_utf8(output).unwrap()
    };

    for agent in [
        "codex",
        "claude-code",
        "claude-desktop",
        "gemini",
        "opencode",
        "qwen",
        "copilot",
        "vscode-copilot",
    ] {
        assert!(text.contains(agent), "missing {agent} in help text");
    }
    assert!(text.contains("mentisdb setup <agent|all>"));
    assert!(text.contains("mentisdb wizard"));
    assert!(text
        .contains("--yes         Apply the rendered plan without the final confirmation prompt"));
}

#[test]
fn setup_prompts_before_writing_and_can_cancel() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("n\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdb", "setup", "codex"],
        &mut input,
        &mut output,
        &mut errors,
    );

    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(errors.is_empty());
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("MentisDB setup plan"));
    assert!(stdout.contains("Apply these setup changes?"));
    assert!(stdout.contains("Cancelled."));
    assert!(!home.join(".codex").join("config.toml").exists());
}

#[test]
fn setup_can_apply_after_confirmation() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("Y\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdb", "setup", "codex"],
        &mut input,
        &mut output,
        &mut errors,
    );

    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(errors.is_empty());
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("MentisDB setup plan"));
    assert!(stdout.contains("Apply these setup changes?"));
    assert!(stdout.contains("Codex ->"));
    let config = std::fs::read_to_string(home.join(".codex").join("config.toml")).unwrap();
    assert!(config.contains("[mcp_servers.mentisdb]"));
}

#[test]
fn setup_yes_applies_without_confirmation_prompt() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdb", "setup", "codex", "--yes"],
        &mut input,
        &mut output,
        &mut errors,
    );

    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(errors.is_empty());
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("MentisDB setup plan"));
    assert!(!stdout.contains("Apply these setup changes?"));
    assert!(stdout.contains("Codex ->"));
}

#[test]
fn parse_node_major_extracts_major_from_standard_versions() {
    assert_eq!(parse_node_major("v22.18.0").unwrap(), 22);
    assert_eq!(parse_node_major("v20.0.0").unwrap(), 20);
    assert_eq!(parse_node_major("v18.17.0").unwrap(), 18);
    assert_eq!(parse_node_major("24.0.0").unwrap(), 24);
}

#[test]
fn parse_node_major_rejects_malformed_versions() {
    assert!(parse_node_major("").is_err());
    assert!(parse_node_major("not-a-version").is_err());
}
