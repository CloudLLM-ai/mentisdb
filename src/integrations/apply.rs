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
        fs::write(path, content)
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
    let setup_plan =
        build_setup_plan_for_integration(integration, url, platform, env).ok_or_else(|| {
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
