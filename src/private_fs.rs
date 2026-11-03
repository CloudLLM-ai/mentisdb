//! Owner-only file and directory creation for data MentisDB writes.
//!
//! A MentisDB data directory holds plaintext thoughts, including the private
//! segments of private chains, vector sidecars, the registry and (optionally)
//! TLS keys inside backups. On a shared machine the process umask (commonly
//! `022`) would leave all of it world-readable, so everything MentisDB creates
//! goes through these helpers:
//!
//! - directories are created `0700` ([`create_dir_all`]),
//! - files are created `0600` ([`options`], [`create`], [`write`]),
//! - both are no-ops on non-Unix targets, where permissions are inherited from
//!   the parent directory's ACL.
//!
//! The mode applies only when the entry is *created*. An existing directory or
//! file is never `chmod`ed by these helpers; a file MentisDB rewrites by
//! writing a temporary sibling and renaming it over the original becomes
//! `0600`, because the temporary sibling is new. [`mode_warning`] tells an
//! operator about an existing directory that other users can open, without
//! changing it.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// `0700`: the mode of every directory MentisDB creates.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// `0600`: the mode of every file MentisDB creates.
#[cfg(unix)]
pub(crate) const FILE_MODE: u32 = 0o600;

/// Create `path` and any missing parents; the final directory, when it is
/// created here, is `0700`.
///
/// Missing *parents* keep the default mode (they are usually a home directory
/// or a mount point that was never MentisDB's to restrict). A directory that
/// already exists is left exactly as it is, whatever its mode.
///
/// # Errors
///
/// Any I/O error from creating a directory.
pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    // `builder` is only mutated on Unix, where the mode is set.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, DIR_MODE);
    match builder.create(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

/// [`OpenOptions`] whose newly created files are `0600`.
///
/// Chain the usual flags (`.create(true).append(true)`) and call `.open`. The
/// mode is applied only when the file does not exist yet.
pub(crate) fn options() -> OpenOptions {
    // `options` is only mutated on Unix, where the mode is set.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, FILE_MODE);
    options
}

/// Create (or truncate) `path` for writing; a new file is `0600`.
///
/// The drop-in for `File::create`.
///
/// # Errors
///
/// Any I/O error from opening the file.
pub(crate) fn create(path: &Path) -> io::Result<File> {
    options().write(true).create(true).truncate(true).open(path)
}

/// Write `contents` to `path`; a new file is `0600`.
///
/// The drop-in for `std::fs::write`.
///
/// # Errors
///
/// Any I/O error from opening or writing the file.
pub(crate) fn write(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    create(path)?.write_all(contents.as_ref())
}

/// Make an existing file `0600` (Unix only); a no-op elsewhere.
///
/// For a file MentisDB itself just wrote over an existing path, such as a
/// backup archive that replaced an older one: `File::create` keeps the mode of
/// the file it truncates, so the mode is set explicitly.
///
/// # Errors
///
/// Any I/O error from reading or changing the file's permissions.
pub(crate) fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(FILE_MODE))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A warning when `dir` exists and group or others can open it (Unix only).
///
/// MentisDB creates its own data directory `0700` but never changes one it
/// finds, so a directory made earlier (or by an older release) may still be
/// `0755`. The text names the directory, its mode and the command that fixes
/// it. `None` when the directory is private, missing, or the platform has no
/// Unix permission bits.
pub(crate) fn mode_warning(dir: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir).ok()?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Some(format!(
                "data directory {} is accessible to other users (mode {mode:03o}); it holds \
                 plaintext private chains and backups. Run `chmod 700 {}`",
                dir.display(),
                dir.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    None
}
