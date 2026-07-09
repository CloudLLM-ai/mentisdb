//! Integration tests for the bearer-token registry (`mentisdb::auth`).
//!
//! Kept outside `src/auth.rs` so production code stays free of test scaffolding.

use mentisdb::auth::{BearerTokenError, BearerTokenRecord, BearerTokenScope, BearerTokenStore};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

fn unique_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "mentisdb_auth_test_{}_{}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

/// Created tokens authorize while the registry stores only their hash.
#[test]
fn created_token_authorizes_without_persisting_plaintext() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);
    let created = store.create("codex", BearerTokenScope::Global).unwrap();

    assert!(store.authorize(&created.token));
    assert!(store.authorize_for_chain(&created.token, "any-chain"));
    assert!(!store.authorize("wrong"));

    let raw_file = fs::read_to_string(store.path()).unwrap();
    assert!(!raw_file.contains(&created.token));

    let _ = fs::remove_dir_all(dir);
}

/// Revoked tokens no longer authorize.
#[test]
fn revoked_token_does_not_authorize() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);
    let created = store.create("codex", BearerTokenScope::Global).unwrap();

    assert!(store.authorize(&created.token));
    store.revoke("codex").unwrap();
    assert!(!store.authorize(&created.token));

    let _ = fs::remove_dir_all(dir);
}

/// Aliases reject empty strings and shell-hostile characters.
#[test]
fn aliases_are_validated() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);

    assert!(store
        .create("codex.laptop-1", BearerTokenScope::Global)
        .is_ok());
    assert!(matches!(
        store.create("bad alias", BearerTokenScope::Global),
        Err(BearerTokenError::InvalidAlias(_))
    ));
    assert!(matches!(
        store.create("", BearerTokenScope::Global),
        Err(BearerTokenError::InvalidAlias(_))
    ));

    let _ = fs::remove_dir_all(dir);
}

/// Global tokens authorize all chains and server-wide operations.
#[test]
fn global_token_authorizes_all_chain_and_global_checks() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);
    let created = store.create("admin", BearerTokenScope::Global).unwrap();

    assert!(store.authorize_for_chain(&created.token, "alpha"));
    assert!(store.authorize_for_chain(&created.token, "beta"));
    assert!(store.authorize_for_chains(&created.token, &["alpha".to_string(), "beta".to_string()]));
    assert!(store.authorize_global(&created.token));

    let _ = fs::remove_dir_all(dir);
}

/// Chain-scoped tokens authorize only their configured chains.
#[test]
fn chain_scoped_token_authorizes_only_matching_chains() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);
    let created = store
        .create(
            "alice",
            BearerTokenScope::chains(["alice-chain", "shared-chain"]).unwrap(),
        )
        .unwrap();

    assert!(store.authorize(&created.token));
    assert!(store.authorize_for_chain(&created.token, "alice-chain"));
    assert!(store.authorize_for_chain(&created.token, "shared-chain"));
    assert!(store.authorize_for_chains(
        &created.token,
        &["alice-chain".to_string(), "shared-chain".to_string()]
    ));
    assert!(!store.authorize_for_chain(&created.token, "private-chain"));
    assert!(!store.authorize_for_chains(
        &created.token,
        &["alice-chain".to_string(), "private-chain".to_string()]
    ));
    assert!(!store.authorize_global(&created.token));
    assert_eq!(
        store.active_scope(&created.token),
        Some(BearerTokenScope::Chains(vec![
            "alice-chain".to_string(),
            "shared-chain".to_string()
        ]))
    );

    let _ = fs::remove_dir_all(dir);
}

/// Delete removes a token from the registry; revoke only marks it inactive.
#[test]
fn delete_removes_token_record_while_revoke_keeps_audit_row() {
    let dir = unique_dir();
    let store = BearerTokenStore::new(&dir);
    store
        .create("keep-audit", BearerTokenScope::Global)
        .unwrap();
    store
        .create("purge-me", BearerTokenScope::chain("alpha").unwrap())
        .unwrap();

    store.revoke("keep-audit").unwrap();
    assert_eq!(store.list().unwrap().len(), 2);
    assert!(!store.list().unwrap()[0].is_active() || !store.list().unwrap()[1].is_active());

    let deleted = store.delete("purge-me").unwrap();
    assert_eq!(deleted.alias, "purge-me");
    let remaining = store.list().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].alias, "keep-audit");
    assert!(remaining[0].revoked_at.is_some());

    store.delete("keep-audit").unwrap();
    assert!(store.list().unwrap().is_empty());

    let _ = fs::remove_dir_all(dir);
}

/// Chain scopes normalize input order, whitespace, and duplicates.
#[test]
fn chain_scopes_deduplicate_sort_and_reject_empty_sets() {
    let scope = BearerTokenScope::chains([" shared ", "alice", "shared"]).unwrap();
    assert_eq!(
        scope,
        BearerTokenScope::Chains(vec!["alice".to_string(), "shared".to_string()])
    );
    assert_eq!(scope.to_string(), "chains:alice,shared");

    assert!(matches!(
        BearerTokenScope::chains(Vec::<String>::new()),
        Err(BearerTokenError::InvalidChainKey(_))
    ));
    assert!(matches!(
        BearerTokenScope::chains(["alice", " "]),
        Err(BearerTokenError::InvalidChainKey(_))
    ));
}

/// Scope strings round-trip through Display and FromStr.
#[test]
fn scopes_roundtrip_through_strings() {
    assert_eq!(
        "global".parse::<BearerTokenScope>().unwrap(),
        BearerTokenScope::Global
    );
    assert_eq!(
        "chain:mentisdb".parse::<BearerTokenScope>().unwrap(),
        BearerTokenScope::Chains(vec!["mentisdb".to_string()])
    );
    assert_eq!(
        "chains:mentisdb,gubatron"
            .parse::<BearerTokenScope>()
            .unwrap(),
        BearerTokenScope::Chains(vec!["gubatron".to_string(), "mentisdb".to_string()])
    );
    assert_eq!(BearerTokenScope::Global.to_string(), "global");
    assert_eq!(
        BearerTokenScope::Chains(vec!["mentisdb".to_string()]).to_string(),
        "chain:mentisdb"
    );
    assert_eq!(
        BearerTokenScope::Chains(vec!["gubatron".to_string(), "mentisdb".to_string()]).to_string(),
        "chains:gubatron,mentisdb"
    );
}

/// Bearer-token records remain compatible with older registry JSON.
#[test]
fn token_records_deserialize_legacy_and_multi_chain_scopes() {
    let legacy_missing_scope = r#"{
            "alias": "old-admin",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null
        }"#;
    let record: BearerTokenRecord = serde_json::from_str(legacy_missing_scope).unwrap();
    assert_eq!(record.scope, BearerTokenScope::Global);

    let legacy_single_chain = r#"{
            "alias": "old-chain",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null,
            "scope": { "type": "chain", "chain_key": "alice" }
        }"#;
    let record: BearerTokenRecord = serde_json::from_str(legacy_single_chain).unwrap();
    assert_eq!(
        record.scope,
        BearerTokenScope::Chains(vec!["alice".to_string()])
    );

    let multi_chain = r#"{
            "alias": "team",
            "token_hash": "hash",
            "created_at": "2026-05-31T00:00:00Z",
            "last_used_at": null,
            "revoked_at": null,
            "scope": { "type": "chains", "chain_keys": ["shared", "alice"] }
        }"#;
    let record: BearerTokenRecord = serde_json::from_str(multi_chain).unwrap();
    assert_eq!(
        record.scope,
        BearerTokenScope::Chains(vec!["alice".to_string(), "shared".to_string()])
    );
}
