//! Integration tests for `.env` file loading and environment variable precedence.
//!
//! These tests exercise the integration between `dotenvy` and
//! `MentisDbServerConfig::from_env()`.  Because `std::env` is a global
//! resource, all tests in this file run **serialised** behind a static
//! mutex so they cannot interfere with one another.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use mentisdb::server::MentisDbServerConfig;

/// Global lock serialising every test that mutates `std::env`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const TEST_VAR_PREFIX: &str = "MENTISDB_TEST_DOTENV_";

fn save_and_clear(key: &str) -> Option<String> {
    let saved = std::env::var(key).ok();
    std::env::remove_var(key);
    saved
}

fn restore(key: &str, value: Option<String>) {
    match value {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn temp_env_file(contents: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    let name = format!("{}env-{}", TEST_VAR_PREFIX, uuid::Uuid::new_v4());
    let path = dir.join(&name);
    let mut file = std::fs::File::create(&path).expect("create temp .env");
    file.write_all(contents.as_bytes())
        .expect("write temp .env");
    path
}

/// Helper: load a .env file and verify the given key has the expected value.
fn assert_env_loaded(path: &PathBuf, key: &str, expected: &str) {
    dotenvy::from_path(path).expect("dotenvy should load temp .env");
    let actual = std::env::var(key).expect("env var should be set after dotenv load");
    assert_eq!(actual, expected, "env var {} should match .env value", key);
}

#[test]
fn dotenvy_loads_mentisdb_dir_from_dotenv() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = "MENTISDB_DIR";
    let original = save_and_clear(key);

    let env_path = temp_env_file("MENTISDB_DIR=/tmp/dotenv-test-dir\n");
    assert_env_loaded(&env_path, key, "/tmp/dotenv-test-dir");

    std::fs::remove_file(&env_path).ok();
    restore(key, original);
}

#[test]
fn dotenvy_loads_mcp_port_from_dotenv() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = "MENTISDB_MCP_PORT";
    let original = save_and_clear(key);

    let env_path = temp_env_file("MENTISDB_MCP_PORT=9999\n");
    assert_env_loaded(&env_path, key, "9999");

    std::fs::remove_file(&env_path).ok();
    restore(key, original);
}

#[test]
fn shell_env_takes_precedence_over_dotenv() {
    let _guard = ENV_LOCK.lock().unwrap();
    // If a variable is already set in the shell, dotenvy must NOT override it.
    let key = "MENTISDB_MCP_PORT";
    let original = save_and_clear(key);

    // Pre-set the shell value.
    std::env::set_var(key, "1111");

    let env_path = temp_env_file("MENTISDB_MCP_PORT=2222\n");
    dotenvy::from_path(&env_path).expect("dotenvy should load without error");

    let actual = std::env::var(key).expect("env var should still be set");
    assert_eq!(
        actual, "1111",
        "shell env var {} must take precedence over .env",
        key
    );

    std::fs::remove_file(&env_path).ok();
    restore(key, original);
}

#[test]
fn missing_dotenv_is_silently_ignored() {
    let _guard = ENV_LOCK.lock().unwrap();
    let nonexistent = std::env::temp_dir().join(format!(
        "{}nonexistent-{}",
        TEST_VAR_PREFIX,
        uuid::Uuid::new_v4()
    ));
    let result = dotenvy::from_path(&nonexistent);
    assert!(
        result.is_err(),
        "loading a missing .env should return an error (which we silently ignore in main)"
    );
}

#[test]
fn config_from_env_reads_dotenv_values() {
    let _guard = ENV_LOCK.lock().unwrap();
    // End-to-end: write a .env, load it, then read the config.
    let dir_key = "MENTISDB_DIR";
    let mcp_key = "MENTISDB_MCP_PORT";
    let rest_key = "MENTISDB_REST_PORT";
    let original_dir = save_and_clear(dir_key);
    let original_mcp = save_and_clear(mcp_key);
    let original_rest = save_and_clear(rest_key);

    let env_path = temp_env_file(
        "MENTISDB_DIR=/tmp/mentisdb-config-test\nMENTISDB_MCP_PORT=11111\nMENTISDB_REST_PORT=22222\n",
    );
    dotenvy::from_path(&env_path).expect("load temp .env");

    let config = MentisDbServerConfig::from_env();
    assert_eq!(
        config.service.chain_dir.display().to_string(),
        "/tmp/mentisdb-config-test"
    );
    assert_eq!(config.mcp_addr.port(), 11111);
    assert_eq!(config.rest_addr.port(), 22222);

    std::fs::remove_file(&env_path).ok();
    restore(dir_key, original_dir);
    restore(mcp_key, original_mcp);
    restore(rest_key, original_rest);
}

#[test]
fn config_from_env_respects_shell_over_dotenv() {
    let _guard = ENV_LOCK.lock().unwrap();
    // Shell env var set before dotenvy load must win.
    let mcp_key = "MENTISDB_MCP_PORT";
    let original_mcp = save_and_clear(mcp_key);

    // Set shell value first.
    std::env::set_var(mcp_key, "7777");

    let env_path = temp_env_file("MENTISDB_MCP_PORT=8888\n");
    dotenvy::from_path(&env_path).expect("load temp .env");

    let config = MentisDbServerConfig::from_env();
    assert_eq!(
        config.mcp_addr.port(),
        7777,
        "config should use shell value, not .env value"
    );

    std::fs::remove_file(&env_path).ok();
    restore(mcp_key, original_mcp);
}
