//! `WebhookUrlPolicy::from_env` reads `MENTISDB_WEBHOOK_ALLOWED_CIDRS`.
//! Kept in its own test binary because it mutates the process environment.
#![cfg(feature = "server")]

use mentisdb::webhooks::{WebhookUrlPolicy, WEBHOOK_ALLOWED_CIDRS_ENV};

/// Unset means strict, a list means `AllowCidrs`, and an invalid list is an
/// error so the daemon refuses to start instead of silently ignoring it.
#[test]
fn test_policy_from_env() {
    assert_eq!(WEBHOOK_ALLOWED_CIDRS_ENV, "MENTISDB_WEBHOOK_ALLOWED_CIDRS");

    std::env::remove_var(WEBHOOK_ALLOWED_CIDRS_ENV);
    assert_eq!(
        WebhookUrlPolicy::from_env().unwrap(),
        WebhookUrlPolicy::Strict
    );

    std::env::set_var(
        WEBHOOK_ALLOWED_CIDRS_ENV,
        "100.64.0.0/10, fd7a:115c:a1e0::/48",
    );
    match WebhookUrlPolicy::from_env().unwrap() {
        WebhookUrlPolicy::AllowCidrs(cidrs) => assert_eq!(cidrs.len(), 2),
        other => panic!("expected AllowCidrs, got {other:?}"),
    }

    std::env::set_var(WEBHOOK_ALLOWED_CIDRS_ENV, "100.64.0.0/10,nope");
    assert!(WebhookUrlPolicy::from_env().is_err());

    std::env::remove_var(WEBHOOK_ALLOWED_CIDRS_ENV);
}
