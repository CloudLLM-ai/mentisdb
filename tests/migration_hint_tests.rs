#![cfg(feature = "server")]
//! Every `mentisdb migrate ...` command a migration message suggests must be
//! accepted by the real CLI parser. The hints once said `mentisdb migrate retry
//! <chain>`, which the parser rejects (it only takes `--chain <key>`), so the
//! advice the 503 body, the quarantine error and the CLI printed did not work.
//! The test pulls the suggested commands out of the messages themselves, so a
//! hint and the parser cannot drift apart again.

use mentisdb::cli::migrate::{local_status, parse_migrate_args};
use mentisdb::migration::fixtures::{semantic_chain, write_fixture, FixtureFormat};
use mentisdb::migration::{
    rollback_chain, run_chain_migration, Failpoint, MigrationBlock, MigrationOptions,
};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use std::path::Path;
use tempfile::TempDir;

fn run(args: &[&str]) -> String {
    let mut argv = vec!["mentisdb"];
    argv.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    // Some commands here fail on purpose (a damaged chain); the tests read
    // the hint printed to stdout, not the exit code.
    let _exit = mentisdb::cli::run_with_io(argv, &mut std::io::empty(), &mut out, &mut err);
    String::from_utf8(out).unwrap()
}

fn put(dir: &Path, key: &str, format: FixtureFormat, count: usize) {
    write_fixture(
        &dir.join(chain_storage_filename(key, StorageAdapterKind::Binary)),
        format,
        &semantic_chain(count),
    )
    .unwrap();
}

/// The arguments after `migrate` of every backtick-quoted `mentisdb migrate ...`
/// (or bare `migrate ...`) command in `message`.
fn suggested_commands(message: &str) -> Vec<Vec<String>> {
    message
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|span| {
            let span = span.strip_prefix("mentisdb ").unwrap_or(span);
            let rest = span.strip_prefix("migrate ")?;
            Some(rest.split_whitespace().map(str::to_owned).collect())
        })
        .collect()
}

/// Assert `message` suggests at least `expected` commands and the CLI parses each.
fn assert_hints_parse(label: &str, message: &str, expected: usize) {
    let commands = suggested_commands(message);
    assert!(
        commands.len() >= expected,
        "{label}: expected {expected} suggested command(s) in {message:?}"
    );
    for command in commands {
        assert!(
            parse_migrate_args(&command).is_ok(),
            "{label}: the suggested `mentisdb migrate {}` is rejected by the CLI ({:?}) in {message:?}",
            command.join(" "),
            parse_migrate_args(&command).err()
        );
    }
}

/// The messages clients and operators see, each parsed.
#[test]
fn test_every_suggested_migrate_command_is_accepted_by_the_cli() {
    // What the 503 body says for each blocked state.
    let key = || "my-chain".to_owned();
    for (label, block) in [
        (
            "quarantined",
            MigrationBlock::Quarantined {
                chain_key: key(),
                error: "boom".into(),
            },
        ),
        ("skipped", MigrationBlock::Skipped { chain_key: key() }),
        (
            "rolled back",
            MigrationBlock::RolledBack { chain_key: key() },
        ),
    ] {
        assert_hints_parse(label, &block.to_string(), 1);
    }

    // The error recorded when a chain keeps crashing the process.
    let dir = TempDir::new().unwrap();
    put(dir.path(), "crashy", FixtureFormat::Schema1, 6);
    for _ in 0..3 {
        let crash = MigrationOptions::crash_at(Failpoint::DuringStage { thought: 2 });
        assert!(run_chain_migration(dir.path(), "crashy", &crash).is_err());
    }
    run_chain_migration(dir.path(), "crashy", &MigrationOptions::default()).unwrap();
    let rows = local_status(dir.path());
    let crashy = rows.iter().find(|r| r.chain_key == "crashy").unwrap();
    assert_hints_parse("crash loop", &crashy.detail, 1);

    // What the CLI itself prints: a failed run, skip, and a rolled-back status.
    let dir = TempDir::new().unwrap();
    put(dir.path(), "bad", FixtureFormat::Schema3Base, 8);
    let path = dir
        .path()
        .join(chain_storage_filename("bad", StorageAdapterKind::Binary));
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    let d = dir.path().to_str().unwrap();
    run(&["migrate", "run", "--dir", d, "--chain", "bad"]);
    assert_hints_parse(
        "quarantined run",
        &run(&["migrate", "run", "--dir", d, "--chain", "bad"]),
        1,
    );
    run(&["migrate", "retry", "--dir", d, "--chain", "bad"]);
    assert_hints_parse(
        "skip",
        &run(&["migrate", "skip", "--dir", d, "--chain", "bad"]),
        1,
    );

    let dir = TempDir::new().unwrap();
    put(dir.path(), "alpha", FixtureFormat::Schema1, 6);
    run_chain_migration(dir.path(), "alpha", &MigrationOptions::default()).unwrap();
    rollback_chain(dir.path(), "alpha", false).unwrap();
    let rows = local_status(dir.path());
    let alpha = rows.iter().find(|r| r.chain_key == "alpha").unwrap();
    assert_eq!(alpha.state, "rolled-back");
    assert_hints_parse("rolled-back status", &alpha.detail, 1);
}
