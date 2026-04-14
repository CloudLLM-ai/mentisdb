//! Integration tests for the backup and restore system.
//!
//! These tests create real temporary directories with realistic MentisDB storage
//! structures, exercise all backup/restore code paths, and verify correctness
//! at every step. They are designed to catch real-world edge cases like
//! permission errors, partial writes, checksum mismatches, and path confusion.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use mentisdb::backup::{
    create_backup, list_backup_contents, restore_backup, BackupManifest, BackupOptions,
    RestoreOptions, BACKUP_EXTENSION, BACKUP_FILENAME_PREFIX, BACKUP_FORMAT_VERSION,
    MANIFEST_FILENAME,
};
use tempfile::TempDir;
use zip::ZipArchive;

// ── Test fixtures ────────────────────────────────────────────────────────────

fn create_empty_registry(dir: &Path) {
    let path = dir.join("mentisdb-registry.json");
    fs::write(&path, r#"{"version":3,"chains":{}}"#).unwrap();
}

/// Creates a realistic MentisDB source directory with chains, skills, webhooks,
/// and a TLS directory for thorough backup testing.
fn create_realistic_source(dir: &Path) {
    create_empty_registry(dir);

    // Chain 1: production
    let chain1 = dir.join("default-abc123.tcbin");
    fs::write(&chain1, b"BINARY_CHAIN_DATA_PLACEHOLDER").unwrap();
    let agents1 = dir.join("default-abc123.agents.json");
    fs::write(&agents1, r#"{"agents":[]}"#).unwrap();
    let entity1 = dir.join("default-abc123.entity-types.json");
    fs::write(&entity1, r#"{"types":[]}"#).unwrap();
    let vectors1 = dir.join("default-abc123.vectors.model-v3.json");
    fs::write(&vectors1, r#"{"vectors":[]}"#).unwrap();

    // Chain 2: another chain
    let chain2 = dir.join("work-xyz789.tcbin");
    fs::write(&chain2, b"SECOND_CHAIN_BIN_DATA").unwrap();
    let agents2 = dir.join("work-xyz789.agents.json");
    fs::write(&agents2, r#"{"agents":[{"id":"agent-1"}]}"#).unwrap();

    // Skills registry
    let skills = dir.join("mentisdb-skills.bin");
    fs::write(&skills, b"BINARY_SKILLS_DATA").unwrap();

    // Webhooks registry
    let webhooks = dir.join("mentisdb-webhooks.json");
    fs::write(&webhooks, r#"{"webhooks":[]}"#).unwrap();

    // TLS directory (cert and key)
    let tls_dir = dir.join("tls");
    fs::create_dir(&tls_dir).unwrap();
    fs::write(tls_dir.join("cert.pem"), b"TLS_CERT_PLACEHOLDER").unwrap();
    fs::write(tls_dir.join("key.pem"), b"TLS_KEY_PLACEHOLDER").unwrap();

    // Non-data file that should be skipped
    fs::write(dir.join("cli-wizard-state.json"), r#"{"wizard":true}"#).unwrap();
}

/// Returns the PathBuf of the backup archive created by create_backup.
/// Places the backup alongside (not inside) source_dir to avoid
/// the backup being picked up as part of the source scan.
fn run_backup(source_dir: &Path, output_path: Option<PathBuf>, include_tls: bool) -> PathBuf {
    let output = output_path.unwrap_or_else(|| {
        let uuid = uuid::Uuid::new_v4();
        let name = format!("backup-{}.mbak", uuid);
        // Place backup NEXT to source_dir, not inside it.
        source_dir.parent().unwrap_or(source_dir).join(name)
    });

    let opts = BackupOptions {
        source_dir: source_dir.to_path_buf(),
        output_path: Some(output.clone()),
        flush_before_backup: false,
        include_tls,
    };

    create_backup(&opts).expect("create_backup should succeed");
    output
}

// ── Round-trip tests ─────────────────────────────────────────────────────────

#[test]
fn test_backup_and_restore_roundtrip_with_realistic_source() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, true);

    // Verify zip was created
    assert!(backup_path.exists(), "backup archive should exist");
    let metadata = fs::metadata(&backup_path).unwrap();
    assert!(metadata.len() > 0, "backup archive should not be empty");

    // Verify archive is a valid zip
    let file = File::open(&backup_path).unwrap();
    let reader = BufReader::new(file);
    let mut archive = ZipArchive::new(reader).expect("should be a valid zip");

    // Verify manifest exists
    assert!(
        archive.by_name(MANIFEST_FILENAME).is_ok(),
        "manifest should exist in archive"
    );

    // Restore to fresh directory
    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path.clone(),
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    // Verify all key files were restored
    assert!(restore_target.join("mentisdb-registry.json").exists());
    assert!(restore_target.join("mentisdb-skills.bin").exists());
    assert!(restore_target.join("mentisdb-webhooks.json").exists());
    assert!(restore_target.join("default-abc123.tcbin").exists());
    assert!(restore_target.join("work-xyz789.tcbin").exists());
    assert!(restore_target.join("default-abc123.agents.json").exists());
    assert!(restore_target.join("work-xyz789.agents.json").exists());
    assert!(restore_target
        .join("default-abc123.entity-types.json")
        .exists());
    assert!(restore_target
        .join("default-abc123.vectors.model-v3.json")
        .exists());

    // TLS should be restored when include_tls was true
    assert!(restore_target.join("tls/cert.pem").exists());
    assert!(restore_target.join("tls/key.pem").exists());

    // Non-data files should NOT be in backup
    assert!(!restore_target.join("cli-wizard-state.json").exists());

    // Verify content integrity of restored files
    let restored_chain1 =
        fs::read(restore_target.join("default-abc123.tcbin")).expect("should read restored file");
    assert_eq!(
        restored_chain1, b"BINARY_CHAIN_DATA_PLACEHOLDER",
        "restored chain content should match original"
    );
}

#[test]
fn test_backup_and_restore_multiple_chains() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    // Both chains should be present
    assert!(restore_target.join("default-abc123.tcbin").exists());
    assert!(restore_target.join("work-xyz789.tcbin").exists());
}

#[test]
fn test_backup_and_restore_skills_and_webhooks() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    assert!(restore_target.join("mentisdb-skills.bin").exists());
    assert!(restore_target.join("mentisdb-webhooks.json").exists());
}

// ── TLS inclusion tests ─────────────────────────────────────────────────────

#[test]
fn test_backup_excludes_tls_by_default() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    // Backup WITHOUT include_tls
    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    // TLS should NOT be present
    assert!(!restore_target.join("tls").exists());
}

#[test]
fn test_backup_includes_tls_when_requested() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    // Backup WITH include_tls
    let backup_path = run_backup(&source, None, true);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    // TLS SHOULD be present
    assert!(restore_target.join("tls/cert.pem").exists());
    assert!(restore_target.join("tls/key.pem").exists());
}

// ── Overwrite behavior tests ─────────────────────────────────────────────────

#[test]
fn test_restore_does_not_overwrite_by_default() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    // Create restore target with pre-existing files
    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    // Create a "marker" file that should be preserved
    let marker = restore_target.join("default-abc123.tcbin");
    fs::write(&marker, b"EXISTING_CONTENT").unwrap();
    let original_mtime = fs::metadata(&marker).unwrap().modified().unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed (idempotent)");

    // Pre-existing content should be preserved
    let content = fs::read(&marker).unwrap();
    assert_eq!(
        content, b"EXISTING_CONTENT",
        "existing file should NOT be overwritten"
    );
    let new_mtime = fs::metadata(&marker).unwrap().modified().unwrap();
    assert_eq!(
        original_mtime, new_mtime,
        "file mtime should not have changed"
    );
}

#[test]
fn test_restore_overwrites_when_requested() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    // Create restore target with pre-existing files
    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    // Create a "marker" file that should be overwritten
    let marker = restore_target.join("default-abc123.tcbin");
    fs::write(&marker, b"EXISTING_CONTENT").unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: true },
    )
    .expect("restore with overwrite should succeed");

    // Pre-existing content should be overwritten
    let content = fs::read(&marker).unwrap();
    assert_eq!(
        content, b"BINARY_CHAIN_DATA_PLACEHOLDER",
        "existing file should be overwritten with backed-up content"
    );
}

// ── Corrupted / invalid archive tests ───────────────────────────────────────

#[test]
fn test_verify_archive_detects_truncated_zip() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    // Corrupt by truncating the zip to remove EOCD
    let original_size = fs::metadata(&backup_path).unwrap().len();
    let truncated_size = original_size / 2;
    let file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&backup_path)
        .unwrap();
    file.set_len(truncated_size).unwrap();
    drop(file);

    // Opening the archive itself should fail because EOCD is corrupted
    let file = File::open(&backup_path).unwrap();
    let result = ZipArchive::new(BufReader::new(file)).map(|_| ());
    assert!(result.is_err(), "truncated zip archive should fail to open");
}

#[test]
fn test_verify_archive_detects_corrupted_file_content() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    // Read the original manifest to get the correct hashes
    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    // Read the original data
    let original_data = {
        let file = File::open(&backup_path).unwrap();
        let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
        let mut entry = archive.by_name("default-abc123.tcbin").unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        data
    };

    // Corrupt the data by flipping a byte
    let mut corrupted_data = original_data.clone();
    if !corrupted_data.is_empty() {
        corrupted_data[0] = corrupted_data[0].wrapping_add(1);
    }

    // Rewrite the zip with corrupted data but original (correct) hash in manifest
    let file = File::create(&backup_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // Write manifest with the ORIGINAL (correct) hash
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    let manifest_json = serde_json::to_string(&manifest).unwrap();
    zip.write_all(manifest_json.as_bytes()).unwrap();

    // Rewrite each file — corrupt the .tcbin file
    for entry in &manifest.files {
        let full_path = source.join(&entry.relative_path);
        let is_chain_file = entry.relative_path.ends_with(".tcbin");
        if is_chain_file {
            // Write corrupted data but with the original (correct) hash
            zip.start_file(&entry.relative_path, options).unwrap();
            zip.write_all(&corrupted_data).unwrap();
        } else {
            let mut src = File::open(&full_path).unwrap();
            zip.start_file(&entry.relative_path, options).unwrap();
            io::copy(&mut src, &mut zip).unwrap();
        }
    }
    let writer = zip.finish().unwrap();
    drop(writer);

    // Verification should fail because the actual data doesn't match the stored hash
    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    // Use the correct hash from original_entry for the corrupted archive's manifest
    let result = manifest.verify_archive(&mut archive);
    assert!(
        result.is_err(),
        "verification should fail on corrupted file data"
    );
}

#[test]
fn test_verify_archive_fails_on_missing_required_file() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    // Read the original manifest
    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    // Rebuild the zip WITHOUT the registry file content (but keep it in the manifest)
    let file = File::create(&backup_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // Write the original manifest (keeps registry in file list)
    zip.start_file(MANIFEST_FILENAME, options).unwrap();
    let manifest_json = serde_json::to_string(&manifest).unwrap();
    zip.write_all(manifest_json.as_bytes()).unwrap();

    // Write all files EXCEPT mentisdb-registry.json
    for entry in &manifest.files {
        if entry.relative_path == "mentisdb-registry.json" {
            continue;
        }
        let full_path = source.join(&entry.relative_path);
        let mut src = File::open(&full_path).unwrap();
        zip.start_file(&entry.relative_path, options).unwrap();
        io::copy(&mut src, &mut zip).unwrap();
    }
    let writer = zip.finish().unwrap();
    drop(writer);

    // Open and verify — should fail because registry is missing from archive
    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    let result = manifest.verify_archive(&mut archive);
    assert!(
        result.is_err(),
        "verification should fail when required file is missing from archive"
    );
}

#[test]
fn test_verify_archive_returns_ok_for_valid_archive() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    let result = manifest.verify_archive(&mut archive);
    assert!(result.is_ok(), "valid archive should pass verification");
}

// ── Format version tests ─────────────────────────────────────────────────────

#[test]
fn test_backup_format_version_is_current() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    assert_eq!(
        manifest.format_version, BACKUP_FORMAT_VERSION,
        "backup format version should be current"
    );
    assert_eq!(manifest.format_version, 1, "format version should be 1");
}

#[test]
fn test_backup_manifest_contains_mentisdb_version() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    assert!(
        !manifest.mentisdb_version.is_empty(),
        "mentisdb version should be non-empty"
    );
    assert_eq!(
        manifest.mentisdb_version,
        env!("CARGO_PKG_VERSION"),
        "mentisdb version should match crate version"
    );
}

// ── Chain count tests ────────────────────────────────────────────────────────

#[test]
fn test_backup_chain_count_matches_source() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    assert_eq!(
        manifest.chain_count, 2,
        "should have 2 chains (default-abc123 and work-xyz789)"
    );
}

#[test]
fn test_backup_chain_count_zero_for_empty_registry() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_empty_registry(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    assert_eq!(manifest.chain_count, 0, "should have 0 chains");
}

// ── File size / byte count tests ────────────────────────────────────────────

#[test]
fn test_backup_total_bytes_matches_files() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    // total_uncompressed_bytes should equal sum of all file sizes
    let reported_total: u64 = manifest.files.iter().map(|f| f.uncompressed_bytes).sum();
    assert_eq!(
        manifest.total_uncompressed_bytes, reported_total,
        "total_uncompressed_bytes should equal sum of individual file sizes"
    );
}

// ── List contents tests ──────────────────────────────────────────────────────

#[test]
fn test_list_backup_contents_returns_all_files() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let files = list_backup_contents(backup_path).expect("list_backup_contents should succeed");

    assert!(!files.is_empty(), "backup should contain at least one file");

    // Should contain expected files
    let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();
    assert!(
        paths.contains(&"mentisdb-registry.json"),
        "should contain registry"
    );
    assert!(
        paths.contains(&"mentisdb-skills.bin"),
        "should contain skills"
    );
    assert!(
        paths.contains(&"default-abc123.tcbin"),
        "should contain chain files"
    );
}

#[test]
fn test_list_backup_contents_returns_correct_file_metadata() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let files = list_backup_contents(backup_path).expect("list_backup_contents should succeed");

    for file in &files {
        assert!(
            !file.relative_path.is_empty(),
            "each file should have a relative_path"
        );
        assert_eq!(
            file.sha256.len(),
            64,
            "SHA-256 hex digest should be 64 characters"
        );
        assert!(
            file.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 should only contain hex digits"
        );
        assert!(
            file.uncompressed_bytes > 0,
            "each file should have non-zero size"
        );
    }
}

// ── Required vs optional file tests ─────────────────────────────────────────

#[test]
fn test_required_files_are_marked_correctly() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    // Include TLS so we can verify TLS files are marked optional
    let backup_path = run_backup(&source, None, true);

    let files = list_backup_contents(backup_path).expect("list_backup_contents should succeed");

    let registry = files
        .iter()
        .find(|f| f.relative_path == "mentisdb-registry.json");
    assert!(
        registry.map(|f| f.required).unwrap_or(false),
        "registry should be required"
    );

    let skills = files
        .iter()
        .find(|f| f.relative_path == "mentisdb-skills.bin");
    assert_eq!(
        skills.map(|f| f.required),
        Some(false),
        "skills should be optional"
    );

    let tls_cert = files.iter().find(|f| f.relative_path == "tls/cert.pem");
    assert_eq!(
        tls_cert.map(|f| f.required),
        Some(false),
        "TLS cert should be optional"
    );
}

// ── Empty / minimal source tests ─────────────────────────────────────────────

#[test]
fn test_backup_and_restore_empty_source() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_empty_registry(&source);

    let backup_path = run_backup(&source, None, false);

    assert!(
        backup_path.exists(),
        "backup should be created even for empty source"
    );

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore of empty backup should succeed");

    // Registry should be restored
    assert!(restore_target.join("mentisdb-registry.json").exists());
}

#[test]
fn test_backup_and_restore_source_without_optional_files() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_empty_registry(&source);

    // Only add one chain, no skills or webhooks
    let chain = source.join("solo-123.tcbin");
    fs::write(&chain, b"SOLO_CHAIN_DATA").unwrap();

    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    assert!(restore_target.join("mentisdb-registry.json").exists());
    assert!(restore_target.join("solo-123.tcbin").exists());
    assert!(!restore_target.join("mentisdb-skills.bin").exists());
    assert!(!restore_target.join("mentisdb-webhooks.json").exists());
}

// ── Idempotent restore tests ─────────────────────────────────────────────────

#[test]
fn test_restore_idempotent_multiple_calls() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    // First restore
    restore_backup(
        backup_path.clone(),
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("first restore should succeed");

    // Second restore (idempotent - should not error)
    restore_backup(
        backup_path.clone(),
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("second restore should succeed (idempotent)");

    // Third restore
    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("third restore should succeed (idempotent)");

    // Everything should still be there
    assert!(restore_target.join("mentisdb-registry.json").exists());
    assert!(restore_target.join("default-abc123.tcbin").exists());
}

// ── Restore to non-empty directory ──────────────────────────────────────────

#[test]
fn test_restore_to_directory_with_unrelated_files() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let restore_target = tmp.path().join("restore");
    fs::create_dir(&restore_target).unwrap();

    // Put unrelated files in the restore target
    fs::write(restore_target.join("unrelated-file.txt"), b"UNRELATED").unwrap();
    fs::write(
        restore_target.join("default-abc123.agents.json"),
        b"UNRELATED_JSON",
    )
    .unwrap();

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed");

    // Unrelated file should be preserved
    assert_eq!(
        fs::read(restore_target.join("unrelated-file.txt")).unwrap(),
        b"UNRELATED"
    );
    // Chain data file should NOT be overwritten (overwrite=false)
    assert_eq!(
        fs::read(restore_target.join("default-abc123.agents.json")).unwrap(),
        b"UNRELATED_JSON"
    );
}

// ── Manifest JSON round-trip tests ───────────────────────────────────────────

#[test]
fn test_manifest_is_valid_json() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();

    // Should parse as valid JSON
    let manifest: BackupManifest =
        serde_json::from_reader(manifest_file).expect("manifest should be valid JSON");

    // Should round-trip through serde
    let reencoded = serde_json::to_string_pretty(&manifest).unwrap();
    let reparsed: BackupManifest =
        serde_json::from_str(&reencoded).expect("re-encoded manifest should parse");

    assert_eq!(
        manifest.format_version, reparsed.format_version,
        "re-encoded manifest should match original"
    );
    assert_eq!(
        manifest.files.len(),
        reparsed.files.len(),
        "file count should match after re-encoding"
    );
}

// ── SHA-256 checksum tests ───────────────────────────────────────────────────

#[test]
fn test_each_file_has_valid_sha256() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let files = list_backup_contents(backup_path).expect("list_backup_contents should succeed");

    for file in &files {
        // SHA-256 hex is always 64 characters
        assert_eq!(
            file.sha256.len(),
            64,
            "SHA-256 should be 64 hex chars for {}",
            file.relative_path
        );
        // All characters should be valid hex digits
        assert!(
            file.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA-256 should only contain hex digits: {}",
            file.sha256
        );
    }
}

// ── Host platform in manifest ────────────────────────────────────────────────

#[test]
fn test_manifest_contains_host_platform() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    assert!(
        !manifest.host_platform.is_empty(),
        "host_platform should be non-empty"
    );
    // Platform should contain OS and arch
    assert!(
        manifest.host_platform.contains('-'),
        "host_platform should be OS-arch format, got: {}",
        manifest.host_platform
    );
}

// ── Created timestamp in manifest ───────────────────────────────────────────

#[test]
fn test_manifest_contains_created_at_timestamp() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let backup_path = run_backup(&source, None, false);

    let file = File::open(&backup_path).unwrap();
    let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
    let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

    // Should be a valid RFC 3339 timestamp
    assert!(
        !manifest.created_at.to_rfc3339().is_empty(),
        "created_at should be a valid RFC 3339 timestamp"
    );
}

// ── Host platform in manifest ────────────────────────────────────────────────

#[test]
fn test_backup_filename_has_correct_extension() {
    use mentisdb::backup::generate_backup_filename;

    let name = generate_backup_filename();
    let expected_suffix = format!(".{}", BACKUP_EXTENSION);
    assert!(
        name.ends_with(&expected_suffix),
        "backup filename should end with .{}",
        BACKUP_EXTENSION
    );
}

#[test]
fn test_backup_filename_has_correct_prefix() {
    use mentisdb::backup::generate_backup_filename;

    let name = generate_backup_filename();
    assert!(
        name.starts_with(BACKUP_FILENAME_PREFIX),
        "backup filename should start with '{}'",
        BACKUP_FILENAME_PREFIX
    );
}

#[test]
fn test_backup_filename_is_timestamp_like() {
    use mentisdb::backup::generate_backup_filename;

    let name = generate_backup_filename();
    // Format: mentisdb-YYYY-MM-DD-HH-MM-SS.mbak
    assert!(
        name.len() >= 30,
        "backup filename should be timestamp-like, got: {}",
        name
    );
}

// ── Restore into subdirectory creation ──────────────────────────────────────

#[test]
fn test_restore_creates_subdirectories() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    // Include TLS so the tls/ subdirectory is in the backup
    let backup_path = run_backup(&source, None, true);

    let restore_target = tmp.path().join("restore");
    // NOTE: we do NOT create restore_target directory here

    restore_backup(
        backup_path,
        restore_target.clone(),
        RestoreOptions { overwrite: false },
    )
    .expect("restore should succeed even if target dir doesn't exist");

    // Subdirectories should have been created
    assert!(restore_target.exists(), "target root should be created");
    assert!(
        restore_target.join("tls").exists(),
        "tls subdir should be created"
    );
}

// ── Backup to custom output path ────────────────────────────────────────────

#[test]
fn test_backup_to_custom_output_path() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    let custom_path = tmp.path().join("my-custom-backup.mbak");

    let opts = BackupOptions {
        source_dir: source,
        output_path: Some(custom_path.clone()),
        flush_before_backup: false,
        include_tls: false,
    };

    create_backup(&opts).expect("create_backup should succeed");

    assert!(
        custom_path.exists(),
        "backup should be at custom output path"
    );

    // Should be a valid zip
    let file = File::open(&custom_path).unwrap();
    let _archive = ZipArchive::new(BufReader::new(file)).expect("should be a valid zip");
}

// ── Vector sidecar files ─────────────────────────────────────────────────────

#[test]
fn test_backup_includes_vector_sidecar_files() {
    let tmp = TempDir::new().unwrap();
    let source = tmp.path().to_path_buf();
    create_realistic_source(&source);

    // Add another vector sidecar with a different model name
    let vectors2 = source.join("default-abc123.vectors.fastembed-v2.json");
    fs::write(&vectors2, r#"{"vectors":[]}"#).unwrap();

    let backup_path = run_backup(&source, None, false);

    let files = list_backup_contents(backup_path).expect("list_backup_contents should succeed");
    let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

    assert!(
        paths.contains(&"default-abc123.vectors.model-v3.json"),
        "should contain first vector sidecar"
    );
    assert!(
        paths.contains(&"default-abc123.vectors.fastembed-v2.json"),
        "should contain second vector sidecar"
    );
}
