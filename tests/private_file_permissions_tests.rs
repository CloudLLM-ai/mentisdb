#![cfg(unix)]
//! MentisDB creates its data owner-only: directories `0700`, files `0600`,
//! backup archives and their entries `0600`.
//!
//! A data directory holds plaintext thoughts (private segments included), the
//! webhook secrets and, in a backup, optionally the TLS private key. Under the
//! common `022` umask the default would be world-readable. Each test sets the
//! umask to `022` first, so a regression to default modes fails here whatever
//! the umask of the machine running the suite.

use mentisdb::backup::{create_backup, restore_backup, BackupOptions, RestoreOptions};
use mentisdb::paths::data_dir_mode_warning;
use mentisdb::webhooks::WebhookManager;
use mentisdb::{MentisDb, ThoughtType};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
type ModeT = u16;
#[cfg(not(target_os = "macos"))]
type ModeT = u32;

extern "C" {
    fn umask(mask: ModeT) -> ModeT;
}

/// Pin the process umask to the common default; a no-op for the assertions
/// except that it makes a regression to default modes visible everywhere.
fn common_umask() {
    // SAFETY: `umask` only swaps the process file-mode creation mask; it has
    // no memory-safety preconditions.
    unsafe { umask(0o022) };
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every entry below `root` (not `root` itself), with its path.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

fn assert_all_private(root: &Path) {
    let entries = walk(root);
    assert!(!entries.is_empty(), "nothing was created under {root:?}");
    for path in entries {
        let expected = if path.is_dir() { 0o700 } else { 0o600 };
        assert_eq!(
            mode(&path),
            expected,
            "{} should be {expected:o}",
            path.display()
        );
    }
}

/// A data directory MentisDB creates, and everything it writes there, is
/// owner-only: the chain file, the registries, the agent registry.
#[test]
fn a_new_data_directory_and_its_chain_files_are_owner_only() {
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    {
        let mut chain = MentisDb::open_with_key(&data, "secret-chain").unwrap();
        chain
            .append("agent", ThoughtType::Finding, "private text")
            .unwrap();
        chain.flush().unwrap();
    }
    assert_eq!(mode(&data), 0o700, "the data directory");
    assert_all_private(&data);
    assert!(
        walk(&data).len() >= 3,
        "chain file, registry and agent registry exist: {:?}",
        walk(&data)
    );
    assert!(data_dir_mode_warning(&data).is_none());
}

/// Webhook registrations hold signing secrets.
#[test]
fn the_webhook_registry_is_owner_only() {
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let manager = WebhookManager::new(data.clone()).unwrap();
    manager
        .register_webhook("https://example.com/hook".to_string(), None, None)
        .unwrap();
    assert_eq!(mode(&data), 0o700);
    assert_all_private(&data);
}

/// An existing directory and the files in it are never changed: MentisDB only
/// secures what it creates (or rewrites). It warns about the directory instead.
#[test]
fn existing_directories_and_files_keep_their_mode() {
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("old-data");
    fs::create_dir(&data).unwrap();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
    let keepsake = data.join("notes.txt");
    fs::write(&keepsake, "mine").unwrap();
    fs::set_permissions(&keepsake, fs::Permissions::from_mode(0o644)).unwrap();

    {
        let mut chain = MentisDb::open_with_key(&data, "chain").unwrap();
        chain.append("agent", ThoughtType::Finding, "text").unwrap();
        chain.flush().unwrap();
    }

    assert_eq!(mode(&data), 0o755, "the existing directory is untouched");
    assert_eq!(mode(&keepsake), 0o644, "a user's own file is untouched");
    for path in walk(&data).into_iter().filter(|p| *p != keepsake) {
        assert_eq!(mode(&path), 0o600, "{} is new, so private", path.display());
    }
    let warning = data_dir_mode_warning(&data).expect("a 0755 data directory warns");
    assert!(warning.contains("chmod 700"), "{warning}");
    assert!(warning.contains(&data.display().to_string()), "{warning}");
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(data_dir_mode_warning(&data).is_none());
    assert!(data_dir_mode_warning(&tmp.path().join("missing")).is_none());
}

fn source_with_a_chain(tmp: &Path) -> PathBuf {
    let source = tmp.join("source");
    {
        let mut chain = MentisDb::open_with_key(&source, "chain").unwrap();
        chain
            .append("agent", ThoughtType::Finding, "to be backed up")
            .unwrap();
        chain.flush().unwrap();
    }
    // A TLS key sits next to the chains, as in a real data directory.
    fs::create_dir(source.join("tls")).unwrap();
    fs::write(source.join("tls/key.pem"), "KEY").unwrap();
    fs::write(source.join("tls/cert.pem"), "CERT").unwrap();
    source
}

/// The archive is `0600`, including when it replaces an older, readable one,
/// and every entry records `0600`; a restore creates owner-only files.
#[test]
fn backup_archives_and_restored_files_are_owner_only() {
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let source = source_with_a_chain(tmp.path());
    let archive = tmp.path().join("out.mentis");

    // A readable archive already sits at the output path.
    fs::write(&archive, "old").unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o644)).unwrap();

    let manifest = create_backup(&BackupOptions {
        source_dir: source.clone(),
        output_path: Some(archive.clone()),
        flush_before_backup: false,
        include_tls: true,
    })
    .unwrap();
    assert_eq!(mode(&archive), 0o600, "the archive replaces a 0644 one");
    assert!(
        manifest
            .files
            .iter()
            .any(|f| f.relative_path == "tls/key.pem"),
        "the TLS key is in the archive: {:?}",
        manifest.files
    );

    let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
    for index in 0..zip.len() {
        let entry = zip.by_index(index).unwrap();
        assert_eq!(
            entry.unix_mode().map(|m| m & 0o777),
            Some(0o600),
            "zip entry {:?}",
            entry.name()
        );
    }
    drop(zip);

    let target = tmp.path().join("restored");
    restore_backup(
        archive.clone(),
        target.clone(),
        RestoreOptions { overwrite: false },
    )
    .unwrap();
    assert_all_private(&target);
    assert_eq!(mode(&target.join("tls/key.pem")), 0o600);
}

/// A backup never carries the operator key (owner decision), and says so.
#[test]
fn backups_exclude_the_operator_key_and_remind_the_operator() {
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let source = source_with_a_chain(tmp.path());
    fs::create_dir(source.join("operator")).unwrap();
    fs::write(source.join("operator/operator.key"), "00".repeat(32)).unwrap();

    let manifest = create_backup(&BackupOptions {
        source_dir: source,
        output_path: Some(tmp.path().join("out.mentis")),
        flush_before_backup: false,
        include_tls: true,
    })
    .unwrap();
    assert!(
        manifest
            .files
            .iter()
            .all(|f| !f.relative_path.contains("operator")),
        "{:?}",
        manifest.files
    );
    let reminder = mentisdb::backup::OPERATOR_KEY_BACKUP_REMINDER;
    assert!(reminder.contains("operator/operator.key"));
    assert!(reminder.contains("separately"));
}

/// `mentisdb backup` prints the operator-key reminder, and the backup and
/// restore help say the same; the archive it wrote is owner-only. (No
/// `--flush`, so the command does not contact a running daemon.)
#[test]
fn the_backup_command_prints_the_operator_key_reminder() {
    use mentisdb::cli::run_with_io;
    common_umask();
    let tmp = tempfile::tempdir().unwrap();
    let source = source_with_a_chain(tmp.path());
    let archive = tmp.path().join("cli.mentis");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with_io(
        [
            "mentisdb",
            "backup",
            "--dir",
            source.to_str().unwrap(),
            "-o",
            archive.to_str().unwrap(),
        ],
        &mut std::io::Cursor::new(""),
        &mut out,
        &mut err,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::SUCCESS),
        "{out}\n{}",
        String::from_utf8_lossy(&err)
    );
    assert!(out.contains("operator/operator.key"), "{out}");
    assert!(out.contains("separately"), "{out}");
    assert_eq!(mode(&archive), 0o600);

    for command in ["backup", "restore"] {
        let (mut help, mut err) = (Vec::new(), Vec::new());
        run_with_io(
            ["mentisdb", command, "--help"],
            &mut std::io::Cursor::new(""),
            &mut help,
            &mut err,
        );
        let help = String::from_utf8(help).unwrap();
        assert!(help.contains("operator/operator.key"), "{command}: {help}");
        assert!(help.contains("0600"), "{command}: {help}");
    }
}
