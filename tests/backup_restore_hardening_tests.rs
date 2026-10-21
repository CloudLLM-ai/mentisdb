//! Restore hardening: declared-size caps, lying archives, and symlink safety.

use mentisdb::backup::{
    restore_backup, BackupFileEntry, BackupManifest, RestoreOptions, BACKUP_FORMAT_VERSION,
    MANIFEST_FILENAME, MAX_RESTORE_ENTRY_BYTES,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

fn manifest_for(files: Vec<BackupFileEntry>) -> BackupManifest {
    BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        mentisdb_version: "test".to_string(),
        created_at: chrono::Utc::now(),
        host_platform: "test".to_string(),
        total_uncompressed_bytes: files.iter().map(|f| f.uncompressed_bytes).sum(),
        files,
        chain_count: 0,
    }
}

fn entry(path: &str, declared: u64, data: &[u8]) -> BackupFileEntry {
    BackupFileEntry {
        relative_path: path.to_string(),
        sha256: format!("{:x}", Sha256::digest(data)),
        uncompressed_bytes: declared,
        required: true,
    }
}

fn write_archive(path: &Path, manifest: &BackupManifest, files: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    zip.write_all(&serde_json::to_vec(manifest).unwrap())
        .unwrap();
    for (name, data) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

/// An entry whose real content exceeds its declared size is rejected and
/// nothing is written to the target.
#[test]
fn test_restore_rejects_entry_larger_than_declared() {
    let tmp = TempDir::new().unwrap();
    let data = vec![b'a'; 4096];
    let manifest = manifest_for(vec![entry("mentisdb-webhooks.json", 10, &data)]);
    let archive = tmp.path().join("lying.mentis");
    write_archive(&archive, &manifest, &[("mentisdb-webhooks.json", &data)]);
    let target = tmp.path().join("target");
    let err = restore_backup(archive, target.clone(), RestoreOptions { overwrite: true })
        .expect_err("restore must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(!target.join("mentisdb-webhooks.json").exists());
}

/// A manifest declaring an absurd per-entry size is rejected before any I/O.
#[test]
fn test_restore_rejects_manifest_over_size_cap() {
    let tmp = TempDir::new().unwrap();
    let data = b"x";
    let manifest = manifest_for(vec![entry(
        "mentisdb-webhooks.json",
        MAX_RESTORE_ENTRY_BYTES + 1,
        data,
    )]);
    let archive = tmp.path().join("huge.mentis");
    write_archive(&archive, &manifest, &[("mentisdb-webhooks.json", data)]);
    let err = restore_backup(
        archive,
        tmp.path().join("target"),
        RestoreOptions { overwrite: true },
    )
    .expect_err("restore must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// A well-formed archive restores and leaves no staging or temp files behind.
#[test]
fn test_restore_valid_archive_leaves_no_staging_artifacts() {
    let tmp = TempDir::new().unwrap();
    let data = br#"{"webhooks":[]}"#;
    let manifest = manifest_for(vec![entry(
        "mentisdb-webhooks.json",
        data.len() as u64,
        data,
    )]);
    let archive = tmp.path().join("ok.mentis");
    write_archive(&archive, &manifest, &[("mentisdb-webhooks.json", data)]);
    let target = tmp.path().join("target");
    restore_backup(archive, target.clone(), RestoreOptions { overwrite: true }).unwrap();
    assert_eq!(
        fs::read(target.join("mentisdb-webhooks.json")).unwrap(),
        data
    );
    let leftovers: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .chain(fs::read_dir(&target).unwrap())
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("restore"))
        .collect();
    assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
}

/// Restore refuses to write through a symlink planted at the destination.
#[cfg(unix)]
#[test]
fn test_restore_does_not_follow_destination_symlink() {
    let tmp = TempDir::new().unwrap();
    let victim = tmp.path().join("victim.txt");
    fs::write(&victim, "precious").unwrap();
    let target = tmp.path().join("target");
    fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&victim, target.join("mentisdb-webhooks.json")).unwrap();

    let data = br#"{"webhooks":[]}"#;
    let manifest = manifest_for(vec![entry(
        "mentisdb-webhooks.json",
        data.len() as u64,
        data,
    )]);
    let archive = tmp.path().join("ok.mentis");
    write_archive(&archive, &manifest, &[("mentisdb-webhooks.json", data)]);
    let result = restore_backup(archive, target, RestoreOptions { overwrite: true });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "precious");
}
