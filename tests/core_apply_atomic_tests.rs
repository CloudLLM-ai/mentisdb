//! Regression tests for atomic, permission-safe integration config writes.

#![cfg(unix)]

use mentisdb::integrations::apply::apply_setup_with_environment;
use mentisdb::integrations::IntegrationKind;
use mentisdb::paths::{HostPlatform, PathEnvironment};
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

/// Rewriting a user config keeps a one-time `.bak`, leaves no temp files, and
/// creates secret-bearing files with owner-only permissions.
#[test]
fn apply_setup_writes_atomically_with_backup_and_private_mode() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let config_path = codex_dir.join("config.toml");
    std::fs::write(&config_path, "model = \"gpt-5.4\"\n").unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let env = PathEnvironment {
        home_dir: Some(home),
        current_dir: Some(temp.path().to_path_buf()),
        ..PathEnvironment::default()
    };
    let apply = |token: Option<String>, url: &str| {
        apply_setup_with_environment(
            IntegrationKind::Codex,
            url.to_string(),
            HostPlatform::Macos,
            &env,
            token,
        )
        .unwrap()
    };

    apply(None, "http://127.0.0.1:9471");
    let backup = codex_dir.join("config.toml.bak");
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "model = \"gpt-5.4\"\n"
    );
    let mode = std::fs::metadata(&config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o644,
        "non-secret writes preserve original permissions"
    );

    apply(Some("secret-token".to_string()), "http://127.0.0.1:9472");
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "model = \"gpt-5.4\"\n",
        "backup is only taken once"
    );
    let rendered = std::fs::read_to_string(&config_path).unwrap();
    assert!(rendered.contains("Bearer secret-token"));
    let mode = std::fs::metadata(&config_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let stray: Vec<_> = std::fs::read_dir(&codex_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(stray.is_empty(), "temp files left behind: {stray:?}");
}
