//! Parity tests for the dashboard Settings catalog ([`crate::dashboard`]).
//!
//! They live outside `dashboard.rs` because a dozen integration-test binaries
//! compile that file by path; tests inside it would run once per binary.

use crate::dashboard::{
    setting_spec, tui_config_line, validate_setting, CLIENT_ONLY_ENV_VARS, DAEMON_SETTINGS,
};
use std::collections::BTreeSet;

/// Every `"MENTISDB_*"` string literal under `src/`, which is how the crate
/// names the variables it reads.
fn env_var_literals_in_src() -> BTreeSet<String> {
    fn walk(dir: &std::path::Path, found: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let mut rest = text.as_str();
                while let Some(start) = rest.find("\"MENTISDB_") {
                    let tail = &rest[start + 1..];
                    let len = tail
                        .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                        .unwrap_or(tail.len());
                    // `len > "MENTISDB_".len()` skips the bare prefix used in
                    // `starts_with` checks.
                    if len > "MENTISDB_".len() && tail[len..].starts_with('"') {
                        found.insert(tail[..len].to_string());
                    }
                    rest = &tail[len..];
                }
            }
        }
    }
    let mut found = BTreeSet::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    found
}

/// Parity guard: a `MENTISDB_*` variable read anywhere in the crate is
/// either on the Settings screen or explicitly listed as client-only.
/// When this fails, add the variable to `DAEMON_SETTINGS` (or, if only the
/// CLI reads it, to `CLIENT_ONLY_ENV_VARS` with the reason).
#[test]
fn settings_catalog_covers_every_env_var_the_crate_reads() {
    let literals = env_var_literals_in_src();
    assert!(
        literals.contains("MENTISDB_DIR"),
        "scanner found nothing: {literals:?}"
    );
    let known: BTreeSet<&str> = DAEMON_SETTINGS
        .iter()
        .map(|spec| spec.name)
        .chain(CLIENT_ONLY_ENV_VARS.iter().map(|(name, _)| *name))
        .collect();
    let missing: Vec<&String> = literals
        .iter()
        .filter(|name| !known.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "read by the crate but missing from the dashboard Settings catalog: {missing:?}"
    );
}

#[test]
fn settings_catalog_names_are_unique_and_disjoint_from_client_only() {
    let mut seen = BTreeSet::new();
    for spec in DAEMON_SETTINGS {
        assert!(spec.name.starts_with("MENTISDB_"), "{}", spec.name);
        assert!(seen.insert(spec.name), "listed twice: {}", spec.name);
        assert!(
            !spec.description.is_empty(),
            "{} has no description",
            spec.name
        );
    }
    for (name, reason) in CLIENT_ONLY_ENV_VARS {
        assert!(
            !seen.contains(name),
            "{name} is both a daemon setting and client-only"
        );
        assert!(!reason.is_empty());
    }
    let aliases: Vec<&str> = DAEMON_SETTINGS
        .iter()
        .flat_map(|s| s.aliases.iter().copied())
        .collect();
    for alias in aliases {
        assert!(
            CLIENT_ONLY_ENV_VARS.iter().any(|(name, _)| *name == alias),
            "alias {alias} should be listed in CLIENT_ONLY_ENV_VARS so it is not shown twice"
        );
    }
}

/// The defaults shown on the screen are the ones the code uses.
#[test]
fn settings_catalog_defaults_match_the_code() {
    let default_of = |name: &str| setting_spec(name).unwrap().default_value;
    assert_eq!(
        default_of("MENTISDB_HNSW_THRESHOLD"),
        crate::search::DEFAULT_EXACT_TO_HNSW_THRESHOLD.to_string()
    );
    assert_eq!(
        default_of("MENTISDB_MAX_BODY_BYTES"),
        crate::server::DEFAULT_MAX_BODY_BYTES.to_string()
    );
    assert_eq!(
        default_of("MENTISDB_BODY_IDLE_TIMEOUT_SECS"),
        crate::server::DEFAULT_BODY_IDLE_TIMEOUT_SECS.to_string()
    );
    assert_eq!(
        default_of("MENTISDB_MIN_BODY_BYTES_PER_SEC"),
        crate::server::DEFAULT_MIN_BODY_BYTES_PER_SEC.to_string()
    );
    assert_eq!(
        default_of("MENTISDB_COLLAPSE_DUPLICATES"),
        crate::lineage::DEFAULT_COLLAPSE_DUPLICATES
            .unwrap()
            .to_string()
    );
    assert_eq!(
        default_of("MENTISDB_VECTOR_FUSION"),
        crate::DEFAULT_VECTOR_FUSION.to_string()
    );
}

/// Settings read once at startup, or cached for the process, must not
/// claim hot reload: the screen would tell the operator a change took
/// effect when the daemon is still using the old value.
#[test]
fn startup_only_settings_are_not_marked_hot_reload() {
    for name in [
        "MENTISDB_VERBOSE",
        "MENTISDB_UPDATE_CHECK",
        "MENTISDB_UPDATE_REPO",
        "MENTISDB_STARTUP_SOUND",
        "MENTISDB_THOUGHT_SOUNDS",
        "MENTISDB_DF_SCOPE",
        "MENTISDB_DEDUP_THRESHOLD",
        "MENTISDB_GROUP_COMMIT_MS",
        "MENTISDB_FSYNC",
        "MENTISDB_EMBEDDINGS",
        // Cached in a `OnceLock` on first use (src/search/hnsw_backend.rs).
        "MENTISDB_HNSW_EF_SEARCH",
    ] {
        assert!(
            !setting_spec(name).unwrap().hot_reload,
            "{name} is read once, not on each use"
        );
    }
}

#[test]
fn secret_settings_never_echo_their_value() {
    let pin = setting_spec("MENTISDB_DASHBOARD_PIN").unwrap();
    assert!(pin.secret);
    let row = pin.to_dashboard_setting();
    assert!(row.value.is_empty());
    assert!(DAEMON_SETTINGS.iter().filter(|s| s.secret).count() == 1);
}

/// The TUI config pane is redrawn after a save. A secret must not be echoed
/// into it, whatever the line looked like before.
#[test]
fn tui_config_line_never_shows_a_secret() {
    for secret in DAEMON_SETTINGS.iter().filter(|s| s.secret) {
        let before = format!("  {}=<unset> (default: not set)", secret.name);
        let line = tui_config_line(secret.name, "hunter2-pin", Some(&before));
        assert!(!line.contains("hunter2-pin"), "{line}");
        assert_eq!(
            line,
            format!("  {}=<hidden> (default: not set)", secret.name)
        );
        assert!(!tui_config_line(secret.name, "hunter2-pin", None).contains("hunter2-pin"));
    }
}

/// Non-secret values are shown, and the `(default: ...)` hint survives the
/// redraw.
#[test]
fn tui_config_line_keeps_value_and_default_hint() {
    let before = "  MENTISDB_UPDATE_REPO=a/b (default: CloudLLM-ai/mentisdb)";
    assert_eq!(
        tui_config_line("MENTISDB_UPDATE_REPO", "x/y", Some(before)),
        "  MENTISDB_UPDATE_REPO=x/y (default: CloudLLM-ai/mentisdb)"
    );
    assert_eq!(
        tui_config_line("MENTISDB_UPDATE_REPO", "x/y", None),
        "  MENTISDB_UPDATE_REPO=x/y"
    );
}

/// Every setting accepts "unset" and its own documented default, rejects
/// text where it wants a number or a boolean, and is accepted only by its own
/// kind: a catalog entry whose default fails its own check would make the
/// Reset button unusable.
#[test]
fn every_setting_accepts_blank_and_its_default_and_rejects_the_wrong_kind() {
    use crate::dashboard::SettingKind;
    for spec in DAEMON_SETTINGS {
        assert!(
            validate_setting(spec.name, "").is_ok(),
            "{} blank",
            spec.name
        );
        let default = spec.default_value;
        // Defaults that are a path, a derived value or prose are not typed values.
        if !default.is_empty() && spec.kind != SettingKind::Text {
            assert!(
                validate_setting(spec.name, default).is_ok(),
                "{} default {default:?}",
                spec.name
            );
        }
        match spec.kind {
            SettingKind::Number => {
                assert!(validate_setting(spec.name, "abc").is_err(), "{}", spec.name)
            }
            SettingKind::Boolean => assert!(
                validate_setting(spec.name, "maybe").is_err(),
                "{}",
                spec.name
            ),
            // Free text is accepted unless the setting has its own check; a
            // secret's check (the PIN) only refuses blank or padded values.
            SettingKind::Text => assert_eq!(
                validate_setting(spec.name, "abc").is_ok(),
                spec.validate.is_none() || spec.secret,
                "{}",
                spec.name
            ),
        }
    }
}
