//! Restore hardening: manifest size cap, file-count cap, central-directory
//! cross-check and free-space check.

use mentisdb::backup::{
    ensure_free_space, restore_backup, BackupFileEntry, BackupManifest, RestoreOptions,
    BACKUP_FORMAT_VERSION, MANIFEST_FILENAME, MAX_RESTORE_FILE_COUNT, MAX_RESTORE_MANIFEST_BYTES,
};
use sha2::{Digest, Sha256};
use std::fs::File;
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
        sha256: Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        uncompressed_bytes: declared,
        required: true,
    }
}

fn write_raw_archive(path: &Path, manifest_json: &[u8], files: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    zip.write_all(manifest_json).unwrap();
    for (name, data) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

fn restore(archive: &Path, tmp: &TempDir) -> std::io::Result<()> {
    restore_backup(
        archive.to_path_buf(),
        tmp.path().join("target"),
        RestoreOptions { overwrite: true },
    )
}

/// A manifest bigger than the cap is rejected without being parsed.
#[test]
fn restore_rejects_oversized_manifest() {
    let tmp = TempDir::new().unwrap();
    let mut json = serde_json::to_vec(&manifest_for(vec![])).unwrap();
    json.pop(); // drop trailing '}'
    json.extend_from_slice(b",\"padding\":\"");
    json.extend(std::iter::repeat_n(
        b'a',
        MAX_RESTORE_MANIFEST_BYTES as usize,
    ));
    json.extend_from_slice(b"\"}");
    let archive = tmp.path().join("bigmanifest.mentis");
    write_raw_archive(&archive, &json, &[]);
    let err = restore(&archive, &tmp).expect_err("must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("manifest larger"), "{err}");
}

/// A manifest declaring more files than the cap is rejected.
#[test]
fn restore_rejects_too_many_files() {
    let tmp = TempDir::new().unwrap();
    let files = (0..=MAX_RESTORE_FILE_COUNT)
        .map(|i| BackupFileEntry {
            relative_path: format!("f{i}"),
            sha256: String::new(),
            uncompressed_bytes: 0,
            required: false,
        })
        .collect();
    let json = serde_json::to_vec(&manifest_for(files)).unwrap();
    assert!(
        json.len() as u64 <= MAX_RESTORE_MANIFEST_BYTES,
        "fixture too big"
    );
    let archive = tmp.path().join("many.mentis");
    write_raw_archive(&archive, &json, &[]);
    let err = restore(&archive, &tmp).expect_err("must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("file limit"), "{err}");
}

/// Declared size that disagrees with the zip central directory is rejected
/// before anything is extracted.
#[test]
fn restore_rejects_declared_size_not_matching_central_directory() {
    let tmp = TempDir::new().unwrap();
    let data = b"hello";
    let manifest = manifest_for(vec![entry("mentisdb-webhooks.json", 1 << 30, data)]);
    let json = serde_json::to_vec(&manifest).unwrap();
    let archive = tmp.path().join("lie.mentis");
    write_raw_archive(&archive, &json, &[("mentisdb-webhooks.json", data)]);
    let err = restore(&archive, &tmp).expect_err("must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("archive directory"), "{err}");
    assert!(!tmp
        .path()
        .join("target")
        .join("mentisdb-webhooks.json")
        .exists());
}

/// An honest archive still restores.
#[test]
fn restore_honest_archive_still_works() {
    let tmp = TempDir::new().unwrap();
    let data = b"[]";
    let manifest = manifest_for(vec![entry(
        "mentisdb-webhooks.json",
        data.len() as u64,
        data,
    )]);
    let archive = tmp.path().join("ok.mentis");
    write_raw_archive(
        &archive,
        &serde_json::to_vec(&manifest).unwrap(),
        &[("mentisdb-webhooks.json", data)],
    );
    restore(&archive, &tmp).unwrap();
    assert!(tmp
        .path()
        .join("target")
        .join("mentisdb-webhooks.json")
        .exists());
}

/// Free-space check refuses an impossible requirement and accepts a trivial one.
#[cfg(unix)]
#[test]
fn free_space_check_rejects_impossible_requirement() {
    let tmp = TempDir::new().unwrap();
    ensure_free_space(tmp.path(), 1).unwrap();
    let err = ensure_free_space(tmp.path(), u64::MAX / 2).expect_err("must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::StorageFull);
}
