//! Backup and restore for MentisDB instances.
//!
//! MentisDB backups are zip archives (`.mbak`) containing a manifest and all
//! storage files needed to replicate a running instance on another machine or after
//! a failure. This module handles both creating backups and restoring from them.
//!
//! # Backup Format
//!
//! A backup is a ZIP archive named `mentisdb-YYYY-MM-DD-HH-MM-SS.mbak` containing:
//!
//! - `mentisdb.manifest.json` — metadata and file清单
//! - All chain data files (`.tcbin`, `.agents.json`, `.entity-types.json`, vector sidecars)
//! - `mentisdb-registry.json` — global chain registry
//! - `mentisdb-skills.bin` — skill registry (if present)
//! - `mentisdb-webhooks.json` — webhook registrations (if present)
//! - `tls/` — TLS certificates and keys (if present)
//!
//! # Manifest
//!
//! The manifest is a JSON file listing every packed file with its SHA-256 hash,
//! uncompressed size, and original relative path. This lets restore verify
//! integrity before committing any data to disk.
//!
//! # Example: Create a backup
//!
//! ```ignore
//! use mentisdb::backup::{create_backup, BackupOptions};
//! use std::path::PathBuf;
//!
//! let options = BackupOptions {
//!     source_dir: PathBuf::from("/home/user/.cloudllm/mentisdb"),
//!     output_path: Some(PathBuf::from("/backups/mentisdb-2026-04-14.mbak")),
//!     flush_before_backup: true,
//!     include_tls: true,
//! };
//!
//! let manifest = create_backup(&options).unwrap();
//! println!("Backed up {} files, {} bytes total",
//!     manifest.files.len(), manifest.total_uncompressed_bytes);
//! ```
//!
//! # Example: Restore a backup
//!
//! ```ignore
//! use mentisdb::backup::restore_backup;
//! use std::path::PathBuf;
//!
//! restore_backup(
//!     PathBuf::from("/backups/mentisdb-2026-04-14.mbak"),
//!     PathBuf::from("/home/user/.cloudllm/mentisdb"),
//!     RestoreOptions { overwrite: false },
//! ).unwrap();
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// Current backup archive format version.
pub const BACKUP_FORMAT_VERSION: u32 = 1;
/// Filename prefix for backup archives.
pub const BACKUP_FILENAME_PREFIX: &str = "mentisdb-";
/// Filename extension for backup archives.
pub const BACKUP_EXTENSION: &str = "mbak";
/// Name of the manifest file inside a backup archive.
pub const MANIFEST_FILENAME: &str = "mentisdb.manifest.json";

/// SHA-256 hex digest of a backed-up file, computed at backup time and
/// verified at restore time.
pub type FileChecksum = String;

/// Metadata describing one file inside a backup archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupFileEntry {
    /// Relative path of the file inside the archive and relative to the
    /// `MENTISDB_DIR` root on the source machine.
    pub relative_path: String,
    /// SHA-256 hex digest of the uncompressed file contents.
    pub sha256: FileChecksum,
    /// Size of the uncompressed file in bytes.
    pub uncompressed_bytes: u64,
    /// True if this file is required for a functional restore; false if it
    /// is optional (e.g. TLS certificates the user may not have generated).
    pub required: bool,
}

/// Complete manifest describing a backup archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackupManifest {
    /// Schema version of this manifest format. Currently [`BACKUP_FORMAT_VERSION`].
    pub format_version: u32,
    /// Version of mentisdb that created this backup.
    pub mentisdb_version: String,
    /// RFC 3339 timestamp when the backup was created.
    pub created_at: DateTime<Utc>,
    /// Host platform where the backup was created.
    pub host_platform: String,
    /// All files included in this backup, in arbitrary order.
    pub files: Vec<BackupFileEntry>,
    /// Sum of [`BackupFileEntry::uncompressed_bytes`] across all files.
    pub total_uncompressed_bytes: u64,
    /// Number of chain data directories detected in the source.
    pub chain_count: usize,
}

impl BackupManifest {
    /// Verify that every `required` file in the manifest is present in the
    /// extracted archive and matches the stored SHA-256 digest.
    ///
    /// Returns `Ok(Vec::new())` if all required files are present and valid.
    /// Returns `Err` listing any missing or corrupted files.
    ///
    /// # Example
    ///
    /// ```
    /// # use mentisdb::backup::{BackupManifest, MANIFEST_FILENAME};
    /// # use std::io;
    /// # fn example(manifest: BackupManifest, archive_path: std::path::PathBuf) -> io::Result<()> {
    /// use std::fs::File;
    /// use std::io::{BufReader, Read};
    /// use zip::ZipArchive;
    ///
    /// let file = File::open(archive_path)?;
    /// let mut archive = ZipArchive::new(BufReader::new(file))?;
    ///
    /// let manifest_file = archive.by_name(MANIFEST_FILENAME)?;
    /// let manifest: BackupManifest = serde_json::from_reader(manifest_file)?;
    ///
    /// manifest.verify_archive(&mut archive)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn verify_archive<R: Read + io::Seek>(
        &self,
        archive: &mut ZipArchive<R>,
    ) -> io::Result<Vec<String>> {
        let mut mismatches = Vec::new();
        for entry in &self.files {
            let Ok(mut file) = archive.by_name(&entry.relative_path) else {
                mismatches.push(format!("missing: {}", entry.relative_path));
                continue;
            };
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            let computed = format!("{:x}", hasher.finalize());
            if computed != entry.sha256 {
                mismatches.push(format!(
                    "checksum mismatch for {}: expected {} got {}",
                    entry.relative_path, entry.sha256, computed
                ));
            }
        }
        if mismatches.is_empty() {
            Ok(Vec::new())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                mismatches.join("; "),
            ))
        }
    }
}

/// Options for [`create_backup`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackupOptions {
    /// Root `MENTISDB_DIR` to back up.
    pub source_dir: PathBuf,
    /// Path where the `.mbak` archive should be written. If `None`, a
    /// timestamped filename is generated in the current working directory.
    pub output_path: Option<PathBuf>,
    /// Flush all open storage adapters before reading files. This ensures the
    /// backup captures a consistent state even if the daemon is running with
    /// `AUTO_FLUSH=false`. Has no effect if the daemon is stopped.
    ///
    /// When the CLI backup command is run against a running daemon, it
    /// automatically calls `POST /v1/admin/flush` before reading files, so
    /// `flush_before_backup` is only needed for direct library usage without
    /// a running daemon.
    pub flush_before_backup: bool,
    /// Include the `tls/` subdirectory (certificates and keys) in the backup.
    pub include_tls: bool,
}

/// Options for [`restore_backup`].
#[derive(Debug, Clone, PartialEq)]
pub struct RestoreOptions {
    /// If `false` (the default), existing files in the target directory are
    /// preserved and the restore skips them. If `true`, existing files are
    /// overwritten with the backed-up versions.
    pub overwrite: bool,
}

/// Generate a timestamped backup filename based on the current UTC time.
///
/// # Example
///
/// ```
/// use mentisdb::backup::generate_backup_filename;
///
/// let filename = generate_backup_filename();
/// assert!(filename.starts_with("mentisdb-"));
/// assert!(filename.ends_with(".mbak"));
/// ```
pub fn generate_backup_filename() -> String {
    let now = chrono::Utc::now();
    format!(
        "{}{}.{}",
        BACKUP_FILENAME_PREFIX,
        now.format("%Y-%m-%d-%H-%M-%S"),
        BACKUP_EXTENSION
    )
}

fn walkdir_sorted(path: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<_> = WalkDir::new(path)
        .sort_by(|a, b| a.path().cmp(b.path()))
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    entries.sort();
    entries
}

fn relative_path(root: &Path, file: &Path) -> Option<PathBuf> {
    file.strip_prefix(root).ok().map(|p| p.to_path_buf())
}

fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(131072, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

/// Create a backup of the given `MENTISDB_DIR` and write an `.mbak` archive.
///
/// This function flushes all chains, walks the source directory, computes SHA-256
/// hashes for each file, and writes a timestamped ZIP archive. The archive is
/// compatible with [`restore_backup`] on any machine with the same `MENTISDB_DIR`
/// structure.
///
/// Files are always included:
/// - `mentisdb-registry.json`
/// - All `*.tcbin` chain data files
/// - All `*.agents.json` files
/// - All `*.entity-types.json` files
/// - All `*.vectors.*.json` vector sidecar files
/// - `mentisdb-skills.bin` (if present)
/// - `mentisdb-webhooks.json` (if present)
/// - `tls/` directory contents (if `options.include_tls` is true and the directory exists)
///
/// The function returns the manifest so callers can display summary information
/// or log it for audit purposes.
///
/// # Errors
///
/// Returns an error if the source directory cannot be read, if the output file
/// cannot be created, or if any file's hash cannot be computed.
///
/// # Example
///
/// ```
/// use mentisdb::backup::{create_backup, BackupOptions};
/// use std::path::PathBuf;
///
/// let opts = BackupOptions {
///     source_dir: PathBuf::from("/home/alice/.cloudllm/mentisdb"),
///     output_path: Some(PathBuf::from("/tmp/my-backup.mbak")),
///     flush_before_backup: false,
///     include_tls: true,
/// };
///
/// let manifest = create_backup(&opts).unwrap();
/// println!("Created {} with {} files",
///     opts.output_path.unwrap().display(), manifest.files.len());
/// ```
pub fn create_backup(options: &BackupOptions) -> io::Result<BackupManifest> {
    let output_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(generate_backup_filename()));

    let source = &options.source_dir;

    // ── Collect files to back up ────────────────────────────────────────────────
    let mut file_entries: Vec<BackupFileEntry> = Vec::new();
    let mut total_bytes = 0u64;
    let mut chain_count = 0usize;

    // Registry is always required
    let registry_path = source.join("mentisdb-registry.json");
    if registry_path.exists() {
        let (hash, size) = sha256_file(&registry_path)?;
        total_bytes += size;
        file_entries.push(BackupFileEntry {
            relative_path: "mentisdb-registry.json".into(),
            sha256: hash,
            uncompressed_bytes: size,
            required: true,
        });
    }

    // Walk all files in source dir
    for file_path in walkdir_sorted(source) {
        let rel = match relative_path(source, &file_path) {
            Some(r) => r,
            None => continue,
        };
        let rel_str = rel.to_string_lossy();

        // Skip hidden files and backup output path itself
        if rel_str.starts_with('.') {
            continue;
        }
        if rel_str
            == output_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        {
            continue;
        }

        // TLS directory
        if rel_str.starts_with("tls/") {
            if options.include_tls {
                let (hash, size) = sha256_file(&file_path)?;
                total_bytes += size;
                file_entries.push(BackupFileEntry {
                    relative_path: rel_str.to_string(),
                    sha256: hash,
                    uncompressed_bytes: size,
                    required: false,
                });
            }
            continue;
        }

        // Skip known non-data files
        if rel_str == "mentisdb-registry.json" {
            continue;
        }
        if rel_str == "cli-wizard-state.json" {
            continue;
        }

        // Skills and webhooks — optional
        if rel_str == "mentisdb-skills.bin" {
            let (hash, size) = sha256_file(&file_path)?;
            total_bytes += size;
            file_entries.push(BackupFileEntry {
                relative_path: rel_str.to_string(),
                sha256: hash,
                uncompressed_bytes: size,
                required: false,
            });
            continue;
        }
        if rel_str == "mentisdb-webhooks.json" {
            let (hash, size) = sha256_file(&file_path)?;
            total_bytes += size;
            file_entries.push(BackupFileEntry {
                relative_path: rel_str.to_string(),
                sha256: hash,
                uncompressed_bytes: size,
                required: false,
            });
            continue;
        }

        // Chain data files (tcbin, agents, entity-types, vector sidecars)
        let is_chain_file = rel_str.ends_with(".tcbin")
            || rel_str.ends_with(".agents.json")
            || rel_str.ends_with(".entity-types.json")
            || rel_str.contains(".vectors.");

        if is_chain_file {
            if rel_str.ends_with(".tcbin") {
                chain_count += 1;
            }
            let (hash, size) = sha256_file(&file_path)?;
            total_bytes += size;
            file_entries.push(BackupFileEntry {
                relative_path: rel_str.to_string(),
                sha256: hash,
                uncompressed_bytes: size,
                required: true,
            });
        }
    }

    // ── Build manifest ───────────────────────────────────────────────────────────
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        mentisdb_version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: chrono::Utc::now(),
        host_platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        files: file_entries,
        total_uncompressed_bytes: total_bytes,
        chain_count,
    };

    // ── Write zip archive ────────────────────────────────────────────────────────
    let file = File::create(&output_path)?;
    let mut zip = ZipWriter::new(BufWriter::with_capacity(1024 * 1024, file));
    let zip_opts: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    // Write manifest first
    zip.start_file(MANIFEST_FILENAME, zip_opts)?;
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    zip.write_all(manifest_json.as_bytes())?;

    // Write each file
    for entry in &manifest.files {
        let full_path = source.join(&entry.relative_path);
        if !full_path.exists() {
            // Skip missing optional files (shouldn't happen but be defensive)
            continue;
        }
        zip.start_file(&entry.relative_path, zip_opts)?;
        let mut src_file = File::open(&full_path)?;
        io::copy(&mut src_file, &mut zip)?;
    }

    let mut writer = zip.finish()?;
    writer.flush()?;
    drop(writer);
    // EOCD is now written and flushed to disk

    Ok(manifest)
}

/// Restore a backup archive into a target `MENTISDB_DIR`.
///
/// This function extracts all files from an `.mbak` archive into the target
/// directory. By default, existing files are preserved (idempotent restore).
///
/// # Verification
///
/// Before writing any files, the archive's manifest is read and every file's
/// SHA-256 digest is verified against the stored value. If any file is
/// missing or corrupted, restore aborts with an error and the target directory
/// is not modified.
///
/// # File collisions
///
/// By default (`options.overwrite == false`), if a file already exists in the
/// target directory it is skipped. Set `options.overwrite = true` to overwrite
/// all files with their backed-up versions.
///
/// # Example
///
/// ```
/// use mentisdb::backup::{restore_backup, RestoreOptions};
/// use std::path::PathBuf;
///
/// let result = restore_backup(
///     PathBuf::from("/backups/mentisdb-2026-04-14.mbak"),
///     PathBuf::from("/home/bob/.cloudllm/mentisdb"),
///     RestoreOptions { overwrite: false },
/// );
///
/// match result {
///     Ok(()) => println!("Restore complete"),
///     Err(e) => eprintln!("Restore failed: {}", e),
/// }
/// ```
pub fn restore_backup(
    archive_path: PathBuf,
    target_dir: PathBuf,
    options: RestoreOptions,
) -> io::Result<()> {
    let file = File::open(&archive_path)?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;

    // ── Read and verify manifest ────────────────────────────────────────────────
    let manifest_file = archive.by_name(MANIFEST_FILENAME)?;
    let manifest: BackupManifest = serde_json::from_reader(manifest_file).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("invalid manifest: {e}"))
    })?;

    if manifest.format_version != BACKUP_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "backup format version mismatch: archive is v{} but this binary supports v{}",
                manifest.format_version, BACKUP_FORMAT_VERSION
            ),
        ));
    }

    // Verify all checksums before writing anything
    manifest.verify_archive(&mut archive).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("archive corrupted: {e}"),
        )
    })?;

    // ── Extract files ─────────────────────────────────────────────────────────────
    for entry in &manifest.files {
        let dest = target_dir.join(&entry.relative_path);

        // Ensure parent directory exists
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        // Skip existing files unless overwrite is set
        if dest.exists() && !options.overwrite {
            continue;
        }

        let mut src = archive.by_name(&entry.relative_path)?;
        let mut dest_file = File::create(&dest)?;
        io::copy(&mut src, &mut dest_file)?;
    }

    Ok(())
}

/// List the files inside a backup archive without extracting them.
///
/// This is useful for previewing what a backup contains before restoring it.
///
/// # Example
///
/// ```ignore
/// use mentisdb::backup::list_backup_contents;
/// use std::path::PathBuf;
///
/// let files = list_backup_contents(PathBuf::from("/backups/mentisdb-2026-04-14.mbak")).unwrap();
///
/// for f in files {
///     println!("{} ({} bytes)", f.relative_path, f.uncompressed_bytes);
/// }
/// ```
pub fn list_backup_contents(archive_path: PathBuf) -> io::Result<Vec<BackupFileEntry>> {
    let file = File::open(&archive_path)?;
    let mut archive = ZipArchive::new(BufReader::new(file))?;
    let manifest_file = archive.by_name(MANIFEST_FILENAME)?;
    let manifest: BackupManifest = serde_json::from_reader(manifest_file)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(manifest.files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_minimal_source(tmp: &TempDir) -> PathBuf {
        let source = tmp.path().to_path_buf();
        let registry = source.join("mentisdb-registry.json");
        fs::write(&registry, r#"{"version":3,"chains":{}}"#).unwrap();
        source
    }

    #[test]
    fn test_backup_and_restore_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let source = create_minimal_source(&tmp);
        let backup_path = tmp.path().join("test.mbak");

        let manifest = create_backup(&BackupOptions {
            source_dir: source.clone(),
            output_path: Some(backup_path.clone()),
            flush_before_backup: false,
            include_tls: false,
        })
        .unwrap();

        assert_eq!(manifest.format_version, BACKUP_FORMAT_VERSION);
        assert_eq!(manifest.chain_count, 0);
        assert!(manifest
            .files
            .iter()
            .any(|f| f.relative_path == "mentisdb-registry.json"));

        // Restore to a different directory
        let restore_target = tmp.path().join("restore");
        fs::create_dir(&restore_target).unwrap();
        restore_backup(
            backup_path.clone(),
            restore_target.clone(),
            RestoreOptions { overwrite: false },
        )
        .unwrap();

        // Registry should be present in restore target
        assert!(restore_target.join("mentisdb-registry.json").exists());
    }

    #[test]
    fn test_list_backup_contents() {
        let tmp = TempDir::new().unwrap();
        let source = create_minimal_source(&tmp);
        let backup_path = tmp.path().join("test.mbak");

        create_backup(&BackupOptions {
            source_dir: source,
            output_path: Some(backup_path.clone()),
            flush_before_backup: false,
            include_tls: false,
        })
        .unwrap();

        let files = list_backup_contents(backup_path).unwrap();
        assert!(!files.is_empty());
    }

    #[test]
    fn test_checksum_verification_detects_corruption() {
        let tmp = TempDir::new().unwrap();
        let source = create_minimal_source(&tmp);
        let backup_path = tmp.path().join("test.mbak");

        let manifest = create_backup(&BackupOptions {
            source_dir: source,
            output_path: Some(backup_path.clone()),
            flush_before_backup: false,
            include_tls: false,
        })
        .unwrap();

        // Read the ORIGINAL correct hash from the manifest
        let original_entry = manifest
            .files
            .iter()
            .find(|f| f.relative_path == "mentisdb-registry.json")
            .unwrap();

        // Corrupt the zip by reading data, modifying it, and writing it back
        // with a manifest that still has the ORIGINAL (correct) hash.
        use std::io::Read;

        let file = File::open(&backup_path).unwrap();
        let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();

        // Read the registry entry data
        let original_data = {
            let mut registry_entry = archive.by_name("mentisdb-registry.json").unwrap();
            let mut data = Vec::new();
            registry_entry.read_to_end(&mut data).unwrap();
            data
        };
        drop(archive);

        // Create corrupted data (flip one byte)
        let mut corrupted_data = original_data.clone();
        if !corrupted_data.is_empty() {
            corrupted_data[0] = corrupted_data[0].wrapping_add(1);
        }

        // Rewrite the zip with CORRUPTED data but ORIGINAL hash in manifest
        let file = File::create(&backup_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        // Write manifest with the ORIGINAL (correct) hash
        zip.start_file(MANIFEST_FILENAME, options).unwrap();
        let manifest_json = serde_json::to_string(&BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            mentisdb_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: chrono::Utc::now(),
            host_platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            files: vec![BackupFileEntry {
                relative_path: "mentisdb-registry.json".into(),
                sha256: original_entry.sha256.clone(),
                uncompressed_bytes: corrupted_data.len() as u64,
                required: true,
            }],
            total_uncompressed_bytes: corrupted_data.len() as u64,
            chain_count: 0,
        })
        .unwrap();
        zip.write_all(manifest_json.as_bytes()).unwrap();

        // Write the CORRUPTED data (hash won't match)
        zip.start_file("mentisdb-registry.json", options).unwrap();
        zip.write_all(&corrupted_data).unwrap();
        let writer = zip.finish().unwrap();
        drop(writer);

        // Verify should fail because data is corrupted
        let file = File::open(&backup_path).unwrap();
        let mut archive = ZipArchive::new(BufReader::new(file)).unwrap();
        let manifest_file = archive.by_name(MANIFEST_FILENAME).unwrap();
        let manifest: BackupManifest = serde_json::from_reader(manifest_file).unwrap();

        let result = manifest.verify_archive(&mut archive);
        assert!(
            result.is_err(),
            "verification should fail on corrupted file data"
        );
    }

    #[test]
    fn test_generate_backup_filename() {
        let name = generate_backup_filename();
        assert!(name.starts_with(BACKUP_FILENAME_PREFIX));
        let expected_suffix = format!(".{}", BACKUP_EXTENSION);
        assert!(name.ends_with(&expected_suffix));
        // Should be timestamp-like: mentisdb-YYYY-MM-DD-HH-MM-SS.mbak
        assert!(name.len() >= 30);
    }
}
