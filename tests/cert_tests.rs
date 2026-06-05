//! Integration tests for the `mentisdb cert` subcommand.
//!
//! The tests cover three axes:
//!
//! 1. **Parser** — `parse_args` accepts the documented flag set and rejects
//!    malformed input. Runs in milliseconds, no I/O.
//! 2. **End-to-end** — `run_with_io(["mentisdb", "cert", "192.0.2.10", ...])`
//!    actually writes a PEM cert + key, embeds the IP as a Subject
//!    Alternative Name, and updates the operator-supplied `.env` file.
//!    Verifies the cert is parseable with `x509-parser` and that the IP
//!    SAN is present in the resulting certificate.
//! 3. **Refuse-to-overwrite** — without `--force`, a second invocation
//!    reuses the existing artifacts (no rewrite of the cert bytes).

use mentisdb::cli::{parse_args, run_with_io, CertCommand, CliCommand};
use std::io::Cursor;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};
use tempfile::tempdir;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn parse(input: &[&str]) -> Result<CliCommand, String> {
    let mut v = vec!["mentisdb".to_string(), "cert".to_string()];
    v.extend(input.iter().map(|s| s.to_string()));
    parse_args(v)
}

#[test]
fn parse_cert_minimal_returns_no_host() {
    let parsed = parse(&[]).unwrap();
    assert_eq!(
        parsed,
        CliCommand::Cert(CertCommand {
            host: None,
            overwrite: false,
            reset: false,
            output_dir: None,
            env_file: None,
            no_env_update: false,
        })
    );
}

#[test]
fn parse_cert_with_host_and_force_and_outdir() {
    let parsed = parse(&["192.0.2.10", "--force", "--out-dir", "/tmp/mentisdb-tls"]).unwrap();
    assert_eq!(
        parsed,
        CliCommand::Cert(CertCommand {
            host: Some("192.0.2.10".to_string()),
            overwrite: true,
            reset: false,
            output_dir: Some("/tmp/mentisdb-tls".to_string()),
            env_file: None,
            no_env_update: false,
        })
    );
}

#[test]
fn parse_cert_dns_name() {
    let parsed = parse(&["vps.example.com"]).unwrap();
    assert_eq!(
        parsed,
        CliCommand::Cert(CertCommand {
            host: Some("vps.example.com".to_string()),
            overwrite: false,
            reset: false,
            output_dir: None,
            env_file: None,
            no_env_update: false,
        })
    );
}

#[test]
fn parse_cert_rejects_two_positional_args() {
    let err = parse(&["a.example.com", "b.example.com"]).unwrap_err();
    assert!(err.contains("at most one positional"), "{err}");
}

#[test]
fn parse_cert_unknown_flag() {
    let err = parse(&["--bogus"]).unwrap_err();
    assert!(err.contains("Unexpected argument"), "{err}");
}

#[test]
fn parse_cert_reset_flag() {
    let parsed = parse(&["--reset"]).unwrap();
    assert_eq!(
        parsed,
        CliCommand::Cert(CertCommand {
            host: None,
            overwrite: true, // --reset implies --force
            reset: true,
            output_dir: None,
            env_file: None,
            no_env_update: false,
        })
    );
}

#[test]
fn parse_cert_reset_with_host() {
    let parsed = parse(&["192.0.2.10", "--reset"]).unwrap();
    assert_eq!(
        parsed,
        CliCommand::Cert(CertCommand {
            host: Some("192.0.2.10".to_string()),
            overwrite: true, // --reset implies --force
            reset: true,
            output_dir: None,
            env_file: None,
            no_env_update: false,
        })
    );
}

#[test]
fn parse_cert_help_short_circuits_to_help() {
    let parsed = parse(&["--help"]).unwrap();
    assert!(matches!(parsed, CliCommand::CertHelp));
}

#[test]
fn run_cert_writes_pem_and_updates_env() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");
    std::fs::write(
        &env_file,
        "MENTISDB_DEFAULT_CHAIN_KEY=demo\n# this is a comment\nMENTISDB_TLS_CERT=/old/cert.pem\n",
    )
    .expect("write initial .env");

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let code = run_with_io(
        [
            "mentisdb",
            "cert",
            "192.0.2.10",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );

    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "stderr: {}",
        String::from_utf8_lossy(&errors)
    );
    let stderr_text = String::from_utf8_lossy(&errors);
    assert!(stderr_text.is_empty(), "stderr: {stderr_text}");

    let stdout_text = String::from_utf8_lossy(&output);
    assert!(
        stdout_text.contains("Wrote new self-signed TLS cert"),
        "stdout was: {stdout_text}"
    );
    assert!(
        stdout_text.contains("192.0.2.10"),
        "stdout was: {stdout_text}"
    );
    assert!(stdout_text.contains("SHA256:"));
    assert!(stdout_text.contains("Restart the daemon"));

    let cert_path = out_dir.join("cert.pem");
    let key_path = out_dir.join("key.pem");
    assert!(
        cert_path.exists(),
        "cert not written: {}",
        cert_path.display()
    );
    assert!(key_path.exists(), "key not written: {}", key_path.display());

    let cert_pem = std::fs::read_to_string(&cert_path).expect("read cert.pem");
    assert!(cert_pem.contains("BEGIN CERTIFICATE"));

    // Cross-check that 192.0.2.10 is in the SAN list by parsing the cert.
    let sans =
        mentisdb::server::ensure_tls_cert_with_sans(&cert_path, &key_path, Vec::new(), false)
            .expect("ensure_tls_cert_with_sans");
    assert!(
        sans.sans.iter().any(|s| s.contains("192.0.2.10")),
        "expected 192.0.2.10 in SAN list, got: {:?}",
        sans.sans
    );

    // The .env file should now point at the new cert and key.
    let updated_env = std::fs::read_to_string(&env_file).expect("read .env");
    assert!(updated_env.contains("MENTISDB_DEFAULT_CHAIN_KEY=demo"));
    assert!(updated_env.contains("# this is a comment"));
    assert!(updated_env.contains("MENTISDB_TLS_CERT="));
    assert!(updated_env.contains(&cert_path.to_string_lossy().into_owned()));
    assert!(!updated_env.contains("/old/cert.pem"));
    assert!(updated_env.contains("MENTISDB_TLS_KEY="));
    assert!(updated_env.contains(&key_path.to_string_lossy().into_owned()));
}

#[test]
fn run_cert_without_force_preserves_existing_cert() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");

    // First run — generate the cert.
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let code = run_with_io(
        [
            "mentisdb",
            "cert",
            "203.0.113.5",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    let cert_path = out_dir.join("cert.pem");
    let bytes_first = std::fs::read(&cert_path).expect("read first cert");

    // Second run — same host, no --force. The cert should be preserved.
    let mut input2 = Cursor::new(Vec::<u8>::new());
    let mut output2 = Vec::new();
    let mut errors2 = Vec::new();
    let code2 = run_with_io(
        [
            "mentisdb",
            "cert",
            "203.0.113.5",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input2,
        &mut output2,
        &mut errors2,
    );
    assert_eq!(code2, ExitCode::SUCCESS);
    let bytes_second = std::fs::read(&cert_path).expect("read second cert");
    assert_eq!(
        bytes_first, bytes_second,
        "cert should be preserved across invocations without --force"
    );

    let stdout2 = String::from_utf8_lossy(&output2);
    assert!(
        stdout2.contains("Reusing existing TLS cert"),
        "expected 'Reusing existing' message; got: {stdout2}"
    );
}

#[test]
fn run_cert_with_force_regenerates() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let _ = run_with_io(
        [
            "mentisdb",
            "cert",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );
    let cert_path = out_dir.join("cert.pem");
    let bytes_first = std::fs::read(&cert_path).expect("read first cert");

    // Sleep a millisecond so mtime/contents can differ.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut input2 = Cursor::new(Vec::<u8>::new());
    let mut output2 = Vec::new();
    let mut errors2 = Vec::new();
    let _ = run_with_io(
        [
            "mentisdb",
            "cert",
            "--force",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input2,
        &mut output2,
        &mut errors2,
    );
    let bytes_second = std::fs::read(&cert_path).expect("read second cert");
    assert_ne!(
        bytes_first, bytes_second,
        "with --force the cert should be regenerated"
    );

    let stdout2 = String::from_utf8_lossy(&output2);
    assert!(
        stdout2.contains("Wrote new self-signed TLS cert"),
        "expected regenerate message; got: {stdout2}"
    );
}

#[test]
fn run_cert_no_env_update_skips_writing_dotenv() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");
    std::fs::write(
        &env_file,
        "MENTISDB_DEFAULT_CHAIN_KEY=demo\nMENTISDB_TLS_CERT=/old/cert.pem\n",
    )
    .expect("seed .env");

    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let code = run_with_io(
        [
            "mentisdb",
            "cert",
            "--no-env-update",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );
    assert_eq!(code, ExitCode::SUCCESS);

    let stdout = String::from_utf8_lossy(&output);
    assert!(
        stdout.contains("export MENTISDB_TLS_CERT="),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("export MENTISDB_TLS_KEY="),
        "stdout: {stdout}"
    );

    let env_after = std::fs::read_to_string(&env_file).expect("read .env");
    assert!(
        env_after.contains("/old/cert.pem"),
        ".env should be untouched when --no-env-update is passed; got: {env_after}"
    );
}

#[test]
fn cert_command_help_flag_in_help_text() {
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let code = run_with_io(
        ["mentisdb", "cert", "--help"],
        &mut input,
        &mut output,
        &mut errors,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    let stdout = String::from_utf8_lossy(&output);
    assert!(stdout.contains("Mint a fresh self-signed TLS certificate"));
    assert!(stdout.contains("--force"));
    assert!(stdout.contains("--reset"));
    assert!(stdout.contains("--out-dir"));
    assert!(stdout.contains("--env-file"));
    assert!(stdout.contains("--no-env-update"));
}

#[test]
fn run_cert_reset_deletes_and_regenerates() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");

    // First run — generate initial cert.
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let code = run_with_io(
        [
            "mentisdb",
            "cert",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    let cert_path = out_dir.join("cert.pem");
    let bytes_first = std::fs::read(&cert_path).expect("read first cert");

    // Sleep a millisecond so mtime/contents can differ.
    std::thread::sleep(std::time::Duration::from_millis(10));

    // Second run with --reset — should delete and regenerate.
    let mut input2 = Cursor::new(Vec::<u8>::new());
    let mut output2 = Vec::new();
    let mut errors2 = Vec::new();
    let code2 = run_with_io(
        [
            "mentisdb",
            "cert",
            "--reset",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input2,
        &mut output2,
        &mut errors2,
    );
    assert_eq!(code2, ExitCode::SUCCESS);
    let bytes_second = std::fs::read(&cert_path).expect("read second cert");
    assert_ne!(
        bytes_first, bytes_second,
        "with --reset the cert should be regenerated"
    );

    let stdout2 = String::from_utf8_lossy(&output2);
    assert!(
        stdout2.contains("Reset to factory defaults"),
        "expected reset message; got: {stdout2}"
    );
    assert!(
        stdout2.contains("generated new self-signed TLS cert"),
        "expected new cert message; got: {stdout2}"
    );
}

#[test]
fn run_cert_reset_with_custom_san() {
    let _guard = env_lock();

    let dir = tempdir().expect("tempdir");
    let out_dir = dir.path().join("tls");
    let env_file = dir.path().join(".env");

    // First run — generate initial cert with one SAN.
    let mut input = Cursor::new(Vec::<u8>::new());
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let _ = run_with_io(
        [
            "mentisdb",
            "cert",
            "203.0.113.5",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input,
        &mut output,
        &mut errors,
    );
    let cert_path = out_dir.join("cert.pem");

    // Second run with --reset and a different SAN — should have new SAN.
    let mut input2 = Cursor::new(Vec::<u8>::new());
    let mut output2 = Vec::new();
    let mut errors2 = Vec::new();
    let _ = run_with_io(
        [
            "mentisdb",
            "cert",
            "198.51.100.7",
            "--reset",
            "--out-dir",
            out_dir.to_string_lossy().as_ref(),
            "--env-file",
            env_file.to_string_lossy().as_ref(),
        ],
        &mut input2,
        &mut output2,
        &mut errors2,
    );

    // Verify the new cert has the new SAN.
    let artifacts = mentisdb::server::ensure_tls_cert_with_sans(
        &cert_path,
        &out_dir.join("key.pem"),
        Vec::new(),
        false,
    )
    .expect("ensure_tls_cert_with_sans");
    assert!(
        artifacts.sans.iter().any(|s| s.contains("198.51.100.7")),
        "expected new SAN 198.51.100.7 in cert after reset, got: {:?}",
        artifacts.sans
    );
    assert!(
        !artifacts.sans.iter().any(|s| s.contains("203.0.113.5")),
        "old SAN 203.0.113.5 should not be present after reset, got: {:?}",
        artifacts.sans
    );
}

#[allow(dead_code)]
fn _path_buf_marker(_: PathBuf) {}
