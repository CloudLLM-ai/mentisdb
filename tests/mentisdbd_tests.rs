#![cfg(feature = "server")]

use std::ffi::OsString;
use std::io::Cursor;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};
use tempfile::tempdir;

#[path = "../src/bin/mentisdbd.rs"]
mod mentisdbd_impl;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn release_core_version_uses_only_the_first_three_numeric_components() {
    assert_eq!(
        mentisdbd_impl::release_core_version("v0.6.0.12"),
        Some([0, 6, 0])
    );
    assert_eq!(
        mentisdbd_impl::release_core_version("0.6.0-beta1"),
        Some([0, 6, 0])
    );
    assert_eq!(mentisdbd_impl::release_core_version("garbage"), None);
}

#[test]
fn release_tag_comparison_ignores_the_fourth_release_counter() {
    assert!(!mentisdbd_impl::release_tag_is_newer("0.6.0.12", "0.6.0"));
    assert!(mentisdbd_impl::release_tag_is_newer("0.6.1.1", "0.6.0"));
    assert!(mentisdbd_impl::release_tag_is_newer("v0.7.0.1", "0.6.9"));
}

#[test]
fn cargo_install_args_target_the_requested_repo_tag_and_binary() {
    let args = mentisdbd_impl::build_cargo_install_args("0.6.0.12", "CloudLLM-ai/mentisdb");
    let expected = vec![
        "install",
        "--git",
        "https://github.com/CloudLLM-ai/mentisdb",
        "--tag",
        "0.6.0.12",
        "--locked",
        "--force",
        "--bin",
        "mentisdbd",
        "mentisdb",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    assert_eq!(args, expected);
}

#[test]
fn update_dialog_box_contains_install_prompt_inside_the_frame() {
    let lines = mentisdbd_impl::build_update_available_lines(
        "0.6.0",
        "0.6.1.14",
        "https://github.com/CloudLLM-ai/mentisdb/releases/tag/0.6.1.14",
    );
    let dialog = mentisdbd_impl::build_ascii_notice_box("mentisdbd update available", &lines);

    assert!(dialog.contains("mentisdbd update available"));
    assert!(dialog.contains("Install release 0.6.1.14 and restart now? [y/N]"));
    assert!(dialog.contains("+"));
}

#[test]
fn update_config_defaults_to_enabled_and_official_repo() {
    let _guard = env_lock();
    std::env::remove_var("MENTISDB_UPDATE_CHECK");
    std::env::remove_var("MENTISDB_UPDATE_REPO");

    let config = mentisdbd_impl::update_config_from_env();
    assert!(config.enabled);
    assert_eq!(config.repo, mentisdbd_impl::DEFAULT_UPDATE_REPO);
}

#[test]
fn update_config_respects_false_flag_and_trimmed_repo_override() {
    let _guard = env_lock();
    std::env::set_var("MENTISDB_UPDATE_CHECK", "off");
    std::env::set_var("MENTISDB_UPDATE_REPO", "  example/mentisdb-fork  ");

    let config = mentisdbd_impl::update_config_from_env();
    assert!(!config.enabled);
    assert_eq!(config.repo, "example/mentisdb-fork");

    std::env::remove_var("MENTISDB_UPDATE_CHECK");
    std::env::remove_var("MENTISDB_UPDATE_REPO");
}

#[test]
fn mentisdbd_help_lists_native_setup_and_wizard_subcommands() {
    let help = mentisdbd_impl::daemon_help_text();
    assert!(help.contains("mentisdbd setup <agent|all>"));
    assert!(help.contains("mentisdbd wizard"));
    assert!(help.contains("mentisdbd --help"));
    for agent in [
        "codex",
        "claude-code",
        "claude-desktop",
        "gemini",
        "opencode",
        "qwen",
        "copilot",
        "vscode-copilot",
        "all",
    ] {
        assert!(help.contains(agent), "missing {agent} from daemon help");
    }
}

#[test]
fn parse_daemon_args_accepts_only_help_or_no_args() {
    assert_eq!(
        mentisdbd_impl::parse_daemon_args(Vec::<OsString>::new()).unwrap(),
        mentisdbd_impl::DaemonArgMode::Run
    );
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("--help")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::Help
    );
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("-h")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::Help
    );
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("help")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::Help
    );
}

#[test]
fn parse_daemon_args_accepts_native_setup_and_wizard_subcommands() {
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("setup"), OsString::from("opencode")])
            .unwrap(),
        mentisdbd_impl::DaemonArgMode::CliSubcommand(vec![
            OsString::from("mentisdbd"),
            OsString::from("setup"),
            OsString::from("opencode"),
        ])
    );

    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("wizard")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::CliSubcommand(vec![
            OsString::from("mentisdbd"),
            OsString::from("wizard"),
        ])
    );
}

#[test]
fn parse_daemon_args_accepts_update_subcommands() {
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("update")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::Update
    );
    assert_eq!(
        mentisdbd_impl::parse_daemon_args([OsString::from("force-update")]).unwrap(),
        mentisdbd_impl::DaemonArgMode::ForceUpdate
    );
}

#[test]
fn parse_daemon_args_rejects_other_unexpected_arguments() {
    let error = mentisdbd_impl::parse_daemon_args([OsString::from("--version")]).unwrap_err();
    assert!(error.contains("Unexpected arguments"));
    assert!(error.contains("--version"));
}

#[test]
fn first_run_setup_notice_only_shows_for_interactive_empty_unconfigured_state() {
    let interactive_first_run = mentisdbd_impl::FirstRunSetupStatus {
        interactive_terminal: true,
        has_registered_chains: false,
        has_configured_integrations: false,
    };
    assert!(mentisdbd_impl::should_show_first_run_setup_notice(
        &interactive_first_run
    ));

    let has_chain = mentisdbd_impl::FirstRunSetupStatus {
        has_registered_chains: true,
        ..interactive_first_run
    };
    assert!(!mentisdbd_impl::should_show_first_run_setup_notice(
        &has_chain
    ));

    let has_configured_integration = mentisdbd_impl::FirstRunSetupStatus {
        has_configured_integrations: true,
        ..interactive_first_run
    };
    assert!(!mentisdbd_impl::should_show_first_run_setup_notice(
        &has_configured_integration
    ));

    let non_interactive = mentisdbd_impl::FirstRunSetupStatus {
        interactive_terminal: false,
        ..interactive_first_run
    };
    assert!(!mentisdbd_impl::should_show_first_run_setup_notice(
        &non_interactive
    ));
}

#[test]
fn first_run_setup_notice_text_points_to_wizard_and_setup_commands() {
    let lines = mentisdbd_impl::build_first_run_setup_lines();
    let dialog = mentisdbd_impl::build_ascii_notice_box("mentisdbd first-run setup", &lines);

    assert!(dialog.contains("mentisdbd first-run setup"));
    assert!(dialog.contains("mentisdbd wizard"));
    assert!(dialog.contains("mentisdbd setup all --dry-run"));
    assert!(dialog.contains("mentisdbd setup <agent>"));
    assert!(dialog.contains("vscode-copilot"));
}

#[test]
fn first_run_setup_can_launch_wizard_from_notice() {
    let status = mentisdbd_impl::FirstRunSetupStatus {
        interactive_terminal: true,
        has_registered_chains: false,
        has_configured_integrations: false,
    };
    let mut input = Cursor::new("Y\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut launched = false;

    let launched_wizard = mentisdbd_impl::maybe_run_first_run_setup_with_io(
        &status,
        &mut input,
        &mut output,
        &mut errors,
        |_input, out, _err| {
            launched = true;
            writeln!(out, "MentisDB setup wizard").unwrap();
            ExitCode::SUCCESS
        },
    )
    .unwrap();

    assert!(launched_wizard);
    assert!(launched);
    assert!(errors.is_empty());
    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("mentisdbd first-run setup"));
    assert!(stdout.contains("Run the MentisDB setup wizard now"));
    assert!(stdout.contains("MentisDB setup wizard"));
}

#[test]
fn first_run_setup_can_be_skipped_from_notice() {
    let status = mentisdbd_impl::FirstRunSetupStatus {
        interactive_terminal: true,
        has_registered_chains: false,
        has_configured_integrations: false,
    };
    let mut input = Cursor::new("n\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut launched = false;

    let launched_wizard = mentisdbd_impl::maybe_run_first_run_setup_with_io(
        &status,
        &mut input,
        &mut output,
        &mut errors,
        |_input, _out, _err| {
            launched = true;
            ExitCode::SUCCESS
        },
    )
    .unwrap();

    assert!(!launched_wizard);
    assert!(!launched);
    assert!(errors.is_empty());
}

#[test]
fn setup_help_uses_the_embedded_mentisdbd_cli_surface() {
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = mentisdbd_impl::run_cli_subcommand_with_io(
        vec![
            OsString::from("mentisdbd"),
            OsString::from("setup"),
            OsString::from("--help"),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );

    assert_eq!(code, ExitCode::SUCCESS);
    assert!(errors.is_empty());

    let stdout = String::from_utf8(output).unwrap();
    assert!(stdout.contains("mentisdbd setup <agent|all>"));
    assert!(stdout.contains("Supported agents (setup/wizard):"));
    assert!(!stdout.contains("mentisdbd daemon"));
}

#[test]
fn daemon_setup_subcommand_renders_first_run_plan_instead_of_daemon_surface() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = mentisdbd_impl::run_cli_subcommand_with_io(
        vec![
            OsString::from("mentisdbd"),
            OsString::from("setup"),
            OsString::from("codex"),
            OsString::from("--dry-run"),
        ],
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
    assert!(!stdout.contains("mentisdbd daemon"));
    assert!(!stdout.contains("Endpoints:"));
}

#[test]
fn daemon_wizard_subcommand_runs_first_run_wizard_flow() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("\n\nn\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = mentisdbd_impl::run_cli_subcommand_with_io(
        vec![OsString::from("mentisdbd"), OsString::from("wizard")],
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
    assert!(stdout.contains("MentisDB setup wizard"));
    assert!(stdout.contains("Apply these setup changes?"));
    assert!(!stdout.contains("mentisdbd daemon"));
    assert!(!stdout.contains("Endpoints:"));
}

#[test]
fn endpoint_catalog_mentions_mcp_resources_and_ranked_search_surfaces() {
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9471));
    let rest = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9472));
    let https_mcp = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9473));
    let https_rest = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9474));

    let catalog =
        mentisdbd_impl::build_endpoint_catalog(addr, rest, Some(https_mcp), Some(https_rest));

    assert!(catalog.contains("mentisdb://skill/core"));
    assert!(catalog.contains("resources/list"));
    assert!(catalog.contains("/v1/lexical-search"));
    assert!(catalog.contains("Ranked lexical search with scores"));
    assert!(catalog.contains("/v1/ranked-search"));
    assert!(catalog.contains("Flat ranked search with optional graph-aware expansion scoring."));
    assert!(catalog.contains("/v1/context-bundles"));
    assert!(catalog.contains("Seed-anchored grouped context bundles for agent reasoning."));
    assert!(catalog.contains("compatibility fallback"));
}

#[cfg(feature = "startup-sound")]
#[test]
fn scheduler_spaces_bursts_without_overlap() {
    let mut scheduler = mentisdbd_impl::ThoughtSoundScheduler::default();

    let first = scheduler.reserve_delay_ms(0, 180);
    let second = scheduler.reserve_delay_ms(0, 120);
    let third = scheduler.reserve_delay_ms(75, 80);

    assert_eq!(first, 0);
    assert_eq!(second, 180 + mentisdbd_impl::THOUGHT_SOUND_GAP_MS);
    assert_eq!(
        third,
        180 + mentisdbd_impl::THOUGHT_SOUND_GAP_MS + 120 + mentisdbd_impl::THOUGHT_SOUND_GAP_MS
            - 75
    );
}

/// No chains: paste line includes bootstrap and skill/core.
#[test]
fn agent_primer_no_chains_shows_bootstrap() {
    let paste_line = mentisdbd_impl::build_agent_primer_paste_line("https://127.0.0.1:9473", false);
    assert!(paste_line.contains("prime yourself for optimal mentisdb usage"));
    assert!(paste_line.contains("127.0.0.1:9473"));
    assert!(paste_line.contains("mentisdb://skill/core"));
    assert!(paste_line.contains("mentisdb_bootstrap"));
}

/// Chains exist: paste line includes bootstrap and skill/core.
#[test]
fn agent_primer_with_chains_shows_resume() {
    let paste_line = mentisdbd_impl::build_agent_primer_paste_line("https://127.0.0.1:9473", true);
    assert!(paste_line.contains("prime yourself for optimal mentisdb usage"));
    assert!(paste_line.contains("127.0.0.1:9473"));
    assert!(paste_line.contains("mentisdb_bootstrap"));
    assert!(paste_line.contains("mentisdb://skill/core"));
    assert!(paste_line.contains("<chain-key>"));
}

/// No-chains paste line uses project-name placeholder, not chain-key.
#[test]
fn agent_primer_no_dashboard() {
    let paste_line = mentisdbd_impl::build_agent_primer_paste_line("https://127.0.0.1:9473", false);
    assert!(paste_line.contains("mentisdb://skill/core"));
    assert!(paste_line.contains("<project-name>"));
}

#[test]
fn update_prompt_empty_input_defaults_to_no() {
    let mut reader = std::io::Cursor::new("\n");
    let mut writer = Vec::new();
    let result = mentisdbd_impl::prompt_yes_no_with_io("Selection", &mut reader, &mut writer)
        .expect("prompt_yes_no_with_io should succeed");
    assert!(!result, "empty input should default to N (false)");
    let output = String::from_utf8(writer).unwrap();
    assert!(output.contains("[y/N]"));
}

#[test]
fn update_prompt_y_returns_true() {
    let mut reader = std::io::Cursor::new("y\n");
    let mut writer = Vec::new();
    let result = mentisdbd_impl::prompt_yes_no_with_io("Selection", &mut reader, &mut writer)
        .expect("prompt_yes_no_with_io should succeed");
    assert!(result, "y input should return true");
}

#[test]
fn update_prompt_n_returns_false() {
    let mut reader = std::io::Cursor::new("n\n");
    let mut writer = Vec::new();
    let result = mentisdbd_impl::prompt_yes_no_with_io("Selection", &mut reader, &mut writer)
        .expect("prompt_yes_no_with_io should succeed");
    assert!(!result, "n input should return false");
}

#[test]
fn update_prompt_invalid_then_enter_returns_false() {
    // First input is invalid ("maybe"), second is empty (default N)
    let mut reader = std::io::Cursor::new("maybe\n\n");
    let mut writer = Vec::new();
    let result = mentisdbd_impl::prompt_yes_no_with_io("Selection", &mut reader, &mut writer)
        .expect("prompt_yes_no_with_io should succeed");
    assert!(
        !result,
        "empty input after invalid should default to N (false)"
    );
    let output = String::from_utf8(writer).unwrap();
    assert!(output.contains("Please type Y or N."));
}
