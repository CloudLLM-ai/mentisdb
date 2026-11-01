//! `mentisdb migrate`: help, parsing, and every subcommand against a real
//! storage directory.

#![cfg(feature = "server")]

use mentisdb::cli::migrate::{local_status, parse_migrate_args, MigrateCommand, MIGRATE_HELP};
use mentisdb::migration::fixtures::{semantic_chain, write_fixture, FixtureFormat};
use mentisdb::{chain_storage_filename, StorageAdapterKind};
use std::path::Path;
use std::process::ExitCode;
use tempfile::TempDir;

fn run(args: &[&str]) -> (ExitCode, String, String) {
    let mut argv = vec!["mentisdb"];
    argv.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = mentisdb::cli::run_with_io(argv, &mut std::io::empty(), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn put(dir: &Path, key: &str, format: FixtureFormat, count: usize) {
    write_fixture(
        &dir.join(chain_storage_filename(key, StorageAdapterKind::Binary)),
        format,
        &semantic_chain(count),
    )
    .unwrap();
}

/// `migrate` is a subcommand and its help documents every verb and the exit status.
#[test]
fn test_migrate_help_covers_every_subcommand() {
    assert!(mentisdb::cli::is_subcommand("migrate"));
    let (code, out, _) = run(&["migrate", "--help"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, MIGRATE_HELP);
    for verb in [
        "status",
        "run",
        "verify",
        "retry",
        "skip",
        "rollback",
        "purge-backups",
        "--dry-run",
        "Exit status",
    ] {
        assert!(out.contains(verb), "help is missing {verb}");
    }
    assert!(mentisdb::cli::help_text().contains("mentisdb migrate"));
}

/// Parsing rejects what it cannot understand with a message, never a panic.
#[test]
fn test_parse_errors_are_clear() {
    let args = |a: &[&str]| a.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert!(parse_migrate_args(&args(&[]))
        .unwrap_err()
        .contains("missing"));
    assert!(parse_migrate_args(&args(&["retry"]))
        .unwrap_err()
        .contains("--chain"));
    assert!(parse_migrate_args(&args(&["run", "--nope"]))
        .unwrap_err()
        .contains("unknown flag"));
    assert!(parse_migrate_args(&args(&["purge-backups", "--older-than-days", "x"])).is_err());
    assert_eq!(
        parse_migrate_args(&args(&["skip", "--chain", "c"])).unwrap(),
        MigrateCommand::Skip {
            dir: None,
            chain: "c".into(),
            reason: "operator request".into()
        }
    );
}

/// status -> dry run -> run -> verify -> rollback -> retry, on one chain.
#[test]
fn test_full_operator_workflow() {
    let dir = TempDir::new().unwrap();
    put(dir.path(), "alpha", FixtureFormat::Schema1, 12);
    put(dir.path(), "beta", FixtureFormat::Current, 3);
    let d = dir.path().to_str().unwrap();

    let (code, out, _) = run(&["migrate", "status", "--dir", d]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("alpha") && out.contains("pending"), "{out}");
    assert!(
        !out.contains("beta"),
        "a current chain needs nothing: {out}"
    );

    let (code, out, _) = run(&["migrate", "run", "--dir", d, "--dry-run"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("alpha: would migrate 12 thoughts"), "{out}");
    assert!(out.contains("beta: current"), "{out}");
    assert!(
        !dir.path().join("migrations").exists(),
        "a dry run writes nothing"
    );

    let (code, out, _) = run(&["migrate", "run", "--dir", d]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("alpha: migrated 12 thoughts"), "{out}");

    let (code, out, _) = run(&["migrate", "verify", "--dir", d, "--chain", "alpha"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("alpha: ok, 12 thoughts"), "{out}");

    let (code, out, _) = run(&["migrate", "status", "--dir", d, "--json"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("\"completed\""), "{out}");

    let (code, out, _) = run(&["migrate", "rollback", "--dir", d, "--chain", "alpha"]);
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    assert_eq!(
        local_status(dir.path())
            .iter()
            .find(|r| r.chain_key == "alpha")
            .unwrap()
            .state,
        "rolled-back"
    );

    let (code, _, _) = run(&["migrate", "retry", "--dir", d, "--chain", "alpha"]);
    assert_eq!(code, ExitCode::SUCCESS);
    let (code, out, _) = run(&["migrate", "run", "--dir", d, "--chain", "alpha"]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("migrated 12 thoughts"), "{out}");

    let (code, out, _) = run(&["migrate", "purge-backups", "--dir", d]);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.contains("removed 1 backup"), "{out}");
}

/// A failing chain makes `run` exit 1 and says how to recover; skip parks it.
#[test]
fn test_failure_exits_nonzero_and_skip_parks_a_chain() {
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

    let (code, out, _) = run(&["migrate", "run", "--dir", d, "--chain", "bad"]);
    assert_eq!(code, ExitCode::from(1));
    assert!(out.contains("bad: FAILED"), "{out}");
    let (code, out, _) = run(&["migrate", "run", "--dir", d, "--chain", "bad"]);
    assert_eq!(code, ExitCode::from(1));
    assert!(out.contains("migrate retry --chain bad"), "{out}");
    let (code, _, _) = run(&["migrate", "status", "--dir", d]);
    assert_eq!(
        code,
        ExitCode::from(1),
        "status exits 1 while a chain is failed"
    );

    let (code, _, _) = run(&["migrate", "retry", "--dir", d, "--chain", "bad"]);
    assert_eq!(code, ExitCode::SUCCESS);
    let (code, _, _) = run(&[
        "migrate", "skip", "--dir", d, "--chain", "bad", "--reason", "later",
    ]);
    assert_eq!(code, ExitCode::SUCCESS);
    let (_, out, _) = run(&["migrate", "status", "--dir", d]);
    assert!(out.contains("skipped") && out.contains("later"), "{out}");
}
