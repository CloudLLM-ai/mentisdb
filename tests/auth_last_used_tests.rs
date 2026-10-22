//! `last_used_at` is buffered in memory and persisted lazily.

use mentisdb::auth::{BearerTokenScope, BearerTokenStore};
use std::time::Duration;

/// Authorizing repeatedly must not rewrite the registry file, yet `list`
/// reports the in-memory usage time and a flush persists it.
#[test]
fn test_authorize_does_not_rewrite_registry_each_request() {
    let dir = tempfile::tempdir().unwrap();
    let store = BearerTokenStore::new(dir.path());
    let created = store.create("agent", BearerTokenScope::Global).unwrap();
    let before = std::fs::read(store.path()).unwrap();

    for _ in 0..50 {
        assert!(store.authorize(&created.token));
    }

    assert_eq!(std::fs::read(store.path()).unwrap(), before);
    assert!(store.list().unwrap()[0].last_used_at.is_some());

    store.flush_last_used().unwrap();
    let reopened = BearerTokenStore::new(dir.path());
    assert!(reopened.list().unwrap()[0].last_used_at.is_some());
}

/// A zero interval restores write-through behaviour.
#[test]
fn test_zero_interval_persists_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let store = BearerTokenStore::new(dir.path()).with_last_used_persist_interval(Duration::ZERO);
    let created = store.create("agent", BearerTokenScope::Global).unwrap();
    assert!(store.authorize(&created.token));
    let reopened = BearerTokenStore::new(dir.path());
    assert!(reopened.list().unwrap()[0].last_used_at.is_some());
}

/// Buffered usage survives a later mutating operation on the registry.
#[test]
fn test_buffered_usage_survives_other_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let store = BearerTokenStore::new(dir.path());
    let created = store.create("one", BearerTokenScope::Global).unwrap();
    assert!(store.authorize(&created.token));
    store.create("two", BearerTokenScope::Global).unwrap();
    let reopened = BearerTokenStore::new(dir.path());
    let one = reopened
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.alias == "one")
        .unwrap();
    assert!(one.last_used_at.is_some());
}

/// `authenticate` resolves scope in one call and rejects bad or revoked tokens.
#[test]
fn test_authenticate_returns_scope_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = BearerTokenStore::new(dir.path());
    let scoped = store
        .create("scoped", BearerTokenScope::chain("alpha").unwrap())
        .unwrap();
    assert_eq!(
        store.authenticate(&scoped.token),
        Some(BearerTokenScope::chain("alpha").unwrap())
    );
    assert_eq!(store.authenticate("nope"), None);
    store.revoke("scoped").unwrap();
    assert_eq!(store.authenticate(&scoped.token), None);
}
