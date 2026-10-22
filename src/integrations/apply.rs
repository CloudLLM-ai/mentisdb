//! File-writing integration setup helpers.

use crate::integrations::files::render_managed_file;
use crate::integrations::plan::{build_setup_plan_for_integration, SetupPlan};
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};
use crate::integrations::targets::build_apply_plan;
use crate::integrations::IntegrationKind;
use crate::paths::{HostPlatform, PathEnvironment};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Filesystem adapter used by integration setup application.
trait FileSystem {
    fn exists(&self, path: &Path) -> bool;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn write_string(&self, path: &Path, content: &str) -> io::Result<()>;
}

#[derive(Debug, Default, Clone, Copy)]
struct OsFileSystem;

impl FileSystem for OsFileSystem {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn write_string(&self, path: &Path, content: &str) -> io::Result<()> {
        write_string_atomic(path, content)
    }
}

/// Write `content` to `path` through a sibling temp file and an atomic rename.
///
/// The temp file is created with owner-only permissions on Unix when the
/// content carries a bearer token, so the secret is never readable by other
/// users even briefly; otherwise the original file's permissions are kept.
/// The first time an existing file is rewritten its original content is saved
/// next to it as `<name>.bak`. Symlinked config files are written through to
/// their target.
fn write_string_atomic(path: &Path, content: &str) -> io::Result<()> {
    use std::io::Write;

    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let existing_perms = fs::metadata(&target).ok().map(|meta| meta.permissions());
    if let Some(perms) = existing_perms.as_ref() {
        write_backup_once(&target, perms)?;
    }

    // Unpredictable sibling name + `create_new` (O_EXCL): a pre-planted file or
    // symlink at the temp path makes the open fail instead of being followed.
    let mut tmp_name = target.clone().into_os_string();
    tmp_name.push(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    let tmp = PathBuf::from(tmp_name);

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = if content.contains("Bearer ") {
            0o600
        } else {
            existing_perms
                .as_ref()
                .map_or(0o644, |perms| perms.mode() & 0o777)
        };
        options.mode(mode);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Heuristic: does this config text look like it carries credentials?
fn looks_like_it_contains_secrets(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "bearer ", "token", "secret", "api_key", "apikey", "password",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Save the original config next to it as `<name>.bak` the first time it is
/// rewritten. The backup is created with `create_new` (never through an
/// existing file or symlink) and owner-only permissions on Unix when the
/// source contains secrets; otherwise it keeps the source permissions.
fn write_backup_once(target: &Path, source_perms: &fs::Permissions) -> io::Result<()> {
    use std::io::Write;

    let mut backup = target.as_os_str().to_os_string();
    backup.push(".bak");
    let backup = PathBuf::from(backup);
    let original = fs::read(target)?;
    let secret = looks_like_it_contains_secrets(&String::from_utf8_lossy(&original));

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(if secret {
            0o600
        } else {
            source_perms.mode() & 0o777
        });
    }
    #[cfg(not(unix))]
    let _ = (secret, source_perms);
    match options.open(&backup) {
        Ok(mut file) => {
            file.write_all(&original)?;
            file.sync_all()
        }
        // Keep the first backup; an existing path (even a symlink) is untouched.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Result of applying MentisDB setup to one integration target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// Integration that was configured.
    pub integration: IntegrationKind,
    /// Canonical config path that was written.
    pub path: PathBuf,
    /// `true` when the file contents changed.
    pub changed: bool,
    /// Follow-up notes to show after applying setup.
    pub notes: Vec<String>,
}

/// Apply setup for one integration using the current process environment.
pub fn apply_setup(integration: IntegrationKind, url: String) -> io::Result<ApplyResult> {
    let env = PathEnvironment::capture();
    let platform = HostPlatform::current();
    apply_setup_with_environment(integration, url, platform, &env, None)
}

/// Apply setup for one integration using an explicit platform and environment snapshot.
pub fn apply_setup_with_environment(
    integration: IntegrationKind,
    url: String,
    platform: HostPlatform,
    env: &PathEnvironment,
    bearer_token: Option<String>,
) -> io::Result<ApplyResult> {
    let setup_plan = build_setup_plan_for_integration(integration, url, platform, env, None)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported integration target",
            )
        })?;
    apply_setup_plan(&setup_plan, &OsFileSystem, bearer_token)
}

fn apply_setup_plan(
    plan: &SetupPlan,
    fs: &impl FileSystem,
    bearer_token: Option<String>,
) -> io::Result<ApplyResult> {
    let writer_settings = IntegrationWriterSettings::default()
        .with_url_for(plan.integration, plan.url.clone())
        .with_bearer_token(bearer_token);
    let apply_plan = build_apply_plan(plan, &writer_settings);
    let changed = apply_integration_plan(&apply_plan, fs)?;

    Ok(ApplyResult {
        integration: plan.integration,
        path: plan.spec.config_target.path.clone(),
        changed,
        notes: plan.notes.clone(),
    })
}

fn apply_integration_plan(plan: &IntegrationApplyPlan, fs: &impl FileSystem) -> io::Result<bool> {
    let mut changed = false;

    for file in &plan.files {
        let existing = if fs.exists(file.path()) {
            Some(fs.read_to_string(file.path())?)
        } else {
            None
        };

        let rendered = render_managed_file(existing.as_deref(), file)?;
        if existing.as_deref() == Some(rendered.as_str()) {
            continue;
        }

        if let Some(parent) = file.path().parent() {
            fs.create_dir_all(parent)?;
        }
        fs.write_string(file.path(), &rendered)?;
        changed = true;
    }

    Ok(changed)
}
