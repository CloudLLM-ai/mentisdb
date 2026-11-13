//! The startup body-limit check and the dashboard's per-value check share one
//! set of ranges and give the same verdict on every value.
//!
//! One test, because it edits process environment variables.

use mentisdb::server::{validate_body_limit_value, validate_body_limits_env};

const NAMES: [&str; 4] = [
    "MENTISDB_MAX_BODY_BYTES",
    "MENTISDB_MAX_INFLIGHT_BODY_BYTES",
    "MENTISDB_BODY_IDLE_TIMEOUT_SECS",
    "MENTISDB_MIN_BODY_BYTES_PER_SEC",
];

/// For each variable and a spread of values, startup validation and
/// `validate_body_limit_value` agree.
#[test]
fn test_startup_and_dashboard_checks_agree() {
    for name in NAMES {
        std::env::remove_var(name);
    }
    for name in NAMES {
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
