//! Chain keys name files and authorization scopes: blank, control-character
//! and oversized keys must never open a chain, through any entry point.
use mentisdb::{validate_chain_key, MentisDb, StorageAdapterKind, MAX_CHAIN_KEY_BYTES};

fn dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("mentisdb_sec_key_{}", uuid::Uuid::new_v4()))
}

fn bad_keys() -> Vec<String> {
    vec![
        String::new(),
        " ".into(),
        "\t\n ".into(),
        "\u{a0}\u{2003}".into(), // Unicode whitespace
        "a\0b".into(),
        "line\nbreak".into(),
        "esc\u{1b}[31m".into(),
        "x".repeat(MAX_CHAIN_KEY_BYTES + 1),
    ]
}

#[test]
fn library_refuses_unnameable_chain_keys() {
    let dir = dir();
    for key in bad_keys() {
        let error =
            MentisDb::open_with_key_and_storage_kind(&dir, &key, StorageAdapterKind::Binary)
                .err()
                .unwrap_or_else(|| panic!("{key:?} must not open a chain"));
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{key:?}");
        assert!(validate_chain_key(&key).is_err(), "{key:?}");
    }
    // Nothing was created.
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ordinary_keys_still_work() {
    let dir = dir();
    for key in [
        "borganism-brain",
        "projet été",
        "a",
        &"k".repeat(MAX_CHAIN_KEY_BYTES),
    ] {
        assert!(validate_chain_key(key).is_ok(), "{key}");
    }
    MentisDb::open_with_key(&dir, "project memory").unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn branch_refuses_unnameable_keys() {
    let dir = dir();
    let mut source = MentisDb::open_with_key(&dir, "source").unwrap();
    let id = source
        .append("a", mentisdb::ThoughtType::Finding, "seed")
        .unwrap()
        .id;
    drop(source);
    for key in bad_keys() {
        assert!(
            MentisDb::branch_from(&dir, "source", id, &key).is_err(),
            "branch key {key:?} accepted"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bearer-token registry is private to the owner.
#[cfg(unix)]
#[test]
fn token_registry_file_is_owner_only() {
    use mentisdb::auth::{BearerTokenScope, BearerTokenStore};
    use std::os::unix::fs::PermissionsExt;
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    let store = BearerTokenStore::new(&dir);
    store.create("owner", BearerTokenScope::Global).unwrap();
    let files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let mode = std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "token registry mode is {mode:o}");
    let _ = std::fs::remove_dir_all(&dir);
}
