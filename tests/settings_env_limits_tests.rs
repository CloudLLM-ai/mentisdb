//! The startup body-limit check and the dashboard's per-value check share one
//! set of ranges, and a blank variable (what an older Settings screen wrote
//! for a cleared field) means "unset", not "not a number".
//!
//! One test, because it edits process environment variables.

use mentisdb::server::{validate_body_limit_value, validate_body_limits_env};

const NAMES: [&str; 4] = [
    "MENTISDB_MAX_BODY_BYTES",
    "MENTISDB_MAX_INFLIGHT_BODY_BYTES",
    "MENTISDB_BODY_IDLE_TIMEOUT_SECS",
    "MENTISDB_MIN_BODY_BYTES_PER_SEC",
];

/// Blank values pass startup validation; bad ones still fail it with the same
/// verdict `validate_body_limit_value` gives the dashboard.
#[test]
fn test_blank_limit_is_unset_and_both_checks_agree() {
    for name in NAMES {
        std::env::remove_var(name);
    }
    for name in NAMES {
        for blank in ["", "   "] {
            std::env::set_var(name, blank);
            validate_body_limits_env().unwrap_or_else(|e| panic!("{name}={blank:?}: {e}"));
        }
        for (value, ok) in [
            ("abc", false),
            ("-5", false),
            ("0", name.ends_with("PER_SEC")),
        ] {
            std::env::set_var(name, value);
            assert_eq!(
                validate_body_limits_env().is_ok(),
                validate_body_limit_value(name, value).is_ok(),
                "{name}={value:?}"
            );
            assert_eq!(
                validate_body_limit_value(name, value).is_ok(),
                ok,
                "{name}={value:?}"
            );
        }
        std::env::remove_var(name);
    }
    assert!(validate_body_limit_value("MENTISDB_DIR", "1").is_err());
}
