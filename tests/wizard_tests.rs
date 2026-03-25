use mentisdb::cli::run_with_io;
use std::io::Cursor;
use std::path::PathBuf;
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
fn wizard_can_apply_default_detected_codex_setup_in_temp_home() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("\n\nY\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdbd", "wizard"],
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
    assert!(stdout.contains("Codex"));
    assert!(stdout.contains("Codex ->"));

    let config = std::fs::read_to_string(home.join(".codex").join("config.toml")).unwrap();
    assert!(config.contains("[mcp_servers.mentisdb]"));
    assert!(config.contains("http://127.0.0.1:9471"));
}

#[test]
fn wizard_can_skip_existing_configured_entry() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    std::fs::write(
        &config_path,
        "[mcp_servers.mentisdb]\nurl = \"http://127.0.0.1:9471\"\n",
    )
    .unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("codex\n\ns\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdbd", "wizard"],
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
    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "[mcp_servers.mentisdb]\nurl = \"http://127.0.0.1:9471\"\n"
    );
}

#[test]
fn wizard_does_not_write_unused_state_file() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new("\n\nY\n");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdbd", "wizard"],
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
    assert!(!PathBuf::from(&home)
        .join(".cloudllm")
        .join("mentisdb")
        .join("cli-wizard-state.json")
        .exists());
}

#[test]
fn wizard_yes_does_not_overwrite_existing_configured_entry() {
    let _guard = env_lock();
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    std::fs::write(
        &config_path,
        "[mcp_servers.mentisdb]\nurl = \"http://127.0.0.1:9471\"\n",
    )
    .unwrap();

    let previous_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        ["mentisdbd", "wizard", "--yes"],
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
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Nothing selected."));
    assert_eq!(
        std::fs::read_to_string(config_path).unwrap(),
        "[mcp_servers.mentisdb]\nurl = \"http://127.0.0.1:9471\"\n"
    );
}
