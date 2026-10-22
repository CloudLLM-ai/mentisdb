//! Integration config writes: no symlink following, owner-only backups.
#![cfg(unix)]

use mentisdb::integrations::apply::apply_setup_with_environment;
use mentisdb::integrations::IntegrationKind;
use mentisdb::paths::{HostPlatform, PathEnvironment};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::{tempdir, TempDir};

fn setup(existing: &str) -> (TempDir, PathEnvironment, PathBuf) {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config = codex_dir.join("config.toml");
    std::fs::write(&config, existing).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    (temp, env, config)
}

fn apply(env: &PathEnvironment, token: Option<&str>) {
    apply_setup_with_environment(
        IntegrationKind::Codex,
        "http://127.0.0.1:9471".to_string(),
        HostPlatform::Macos,
        env,
        token.map(str::to_string),
    )
    .unwrap();
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A backup of a config containing secrets is owner-only; a plain one keeps
/// the source permissions.
#[test]
fn backup_is_owner_only_when_source_has_secrets() {
    let original = "model = \"x\"\n[mcp_servers.other]\ntoken = \"hunter2\"\n";
    let (_temp, env, config) = setup(original);
    apply(&env, None);
    let bak = PathBuf::from(format!("{}.bak", config.display()));
    assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
    assert_eq!(mode(&bak), 0o600);

    let (_temp2, env2, config2) = setup("model = \"x\"\n");
    apply(&env2, None);
    let bak2 = PathBuf::from(format!("{}.bak", config2.display()));
    assert_eq!(mode(&bak2), 0o644);
}

/// A symlink planted at the `.bak` path is not written through.
#[test]
fn backup_does_not_follow_planted_symlink() {
    let (temp, env, config) = setup("model = \"x\"\n");
    let victim = temp.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    let bak = PathBuf::from(format!("{}.bak", config.display()));
    std::os::unix::fs::symlink(&victim, &bak).unwrap();
    apply(&env, None);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
}

/// The old predictable `<name>.tmp-<pid>` path is no longer used, so a symlink
/// planted there is never followed or clobbered.
#[test]
fn temp_file_is_unpredictable_and_not_symlink_followed() {
    let (temp, env, config) = setup("model = \"x\"\n");
    let victim = temp.path().join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    let planted = PathBuf::from(format!("{}.tmp-{}", config.display(), std::process::id()));
    std::os::unix::fs::symlink(&victim, &planted).unwrap();
    apply(&env, None);
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    assert!(std::fs::symlink_metadata(&planted).unwrap().is_symlink());
    assert!(std::fs::read_to_string(&config)
        .unwrap()
        .contains("mcp_servers"));
}

/// A config that receives a bearer token is written owner-only.
#[test]
fn config_with_bearer_token_is_owner_only() {
    let (_temp, env, config) = setup("model = \"x\"\n");
    apply(&env, Some("tok_secret_value"));
    assert_eq!(mode(&config), 0o600);
}
