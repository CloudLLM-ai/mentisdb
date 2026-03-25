#![cfg(feature = "server")]

use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};

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
    assert!(dialog.contains("Install release 0.6.1.14 and restart now? [Y/N]"));
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
fn mentisdbd_help_mentions_separate_setup_and_wizard_binary() {
    let help = mentisdbd_impl::daemon_help_text();
    assert!(help.contains("mentisdb setup <agent|all>"));
    assert!(help.contains("mentisdb wizard"));
    assert!(help.contains("mentisdb setup --help"));
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
fn parse_daemon_args_rejects_setup_and_wizard_subcommands() {
    let setup =
        mentisdbd_impl::parse_daemon_args([OsString::from("setup"), OsString::from("opencode")])
            .unwrap_err();
    assert!(setup.contains("mentisdbd setup opencode"));
    assert!(setup.contains("mentisdb setup opencode"));

    let wizard = mentisdbd_impl::parse_daemon_args([OsString::from("wizard")]).unwrap_err();
    assert!(wizard.contains("mentisdbd wizard"));
    assert!(wizard.contains("mentisdb wizard"));
}

#[test]
fn parse_daemon_args_rejects_other_unexpected_arguments() {
    let error = mentisdbd_impl::parse_daemon_args([OsString::from("--version")]).unwrap_err();
    assert!(error.contains("Unexpected arguments"));
    assert!(error.contains("--version"));
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
