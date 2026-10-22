//! Cross-platform path resolution helpers for MentisDB and host integrations.
//!
//! The setup and wizard flows need deterministic, testable rules for common
//! user-level configuration directories. This module centralizes those rules so
//! daemon code, CLI code, and setup planning share the same path semantics.

use std::path::PathBuf;

const MENTISDB_DIRNAME: &str = "mentisdb";

/// Operating-system family used for integration path planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostPlatform {
    /// Apple macOS.
    Macos,
    /// Linux and other Unix systems that follow XDG-style config roots.
    Linux,
    /// Microsoft Windows.
    Windows,
    /// Any unsupported or unknown platform.
    Other,
}

impl HostPlatform {
    /// Return the host platform for the current process.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }

    /// Return a stable lowercase identifier suitable for logs and JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
            Self::Other => "other",
        }
    }
}

/// Snapshot of environment-derived path inputs used to resolve config roots.
///
/// This struct makes platform path logic deterministic in tests and reusable by
/// future CLI/setup flows without depending directly on global process state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathEnvironment {
    /// Explicit override for the MentisDB storage root (`MENTISDB_DIR`).
    pub mentisdb_dir_override: Option<PathBuf>,
    /// POSIX-style home directory (`HOME`).
    pub home_dir: Option<PathBuf>,
    /// Windows-style profile root (`USERPROFILE`).
    pub user_profile: Option<PathBuf>,
    /// XDG config directory (`XDG_CONFIG_HOME`).
    pub xdg_config_home: Option<PathBuf>,
    /// Windows roaming application data directory (`APPDATA`).
    pub app_data: Option<PathBuf>,
    /// Windows local application data directory (`LOCALAPPDATA`).
    pub local_app_data: Option<PathBuf>,
    /// Current working directory used as the last fallback.
    pub current_dir: Option<PathBuf>,
}

impl PathEnvironment {
    /// Capture relevant path inputs from the current process environment.
    pub fn capture() -> Self {
        Self {
            mentisdb_dir_override: non_empty_var("MENTISDB_DIR"),
            home_dir: non_empty_var("HOME"),
            user_profile: non_empty_var("USERPROFILE"),
            xdg_config_home: non_empty_var("XDG_CONFIG_HOME"),
            app_data: non_empty_var("APPDATA"),
            local_app_data: non_empty_var("LOCALAPPDATA"),
            current_dir: std::env::current_dir().ok(),
        }
    }

    /// Return the most appropriate home directory for a platform.
    pub fn home_dir_for(&self, platform: HostPlatform) -> Option<PathBuf> {
        match platform {
            HostPlatform::Windows => self.user_profile.clone().or_else(|| self.home_dir.clone()),
            _ => self.home_dir.clone().or_else(|| self.user_profile.clone()),
        }
    }

    /// Return the user-scoped configuration root for a platform.
    ///
    /// On macOS this is `~/Library/Application Support`, on Linux/XDG it is
    /// `XDG_CONFIG_HOME` or `~/.config`, and on Windows it is `APPDATA` when
    /// available.
    pub fn config_root_for(&self, platform: HostPlatform) -> Option<PathBuf> {
        match platform {
            HostPlatform::Macos => self
                .home_dir_for(platform)
                .map(|home| home.join("Library").join("Application Support")),
            HostPlatform::Linux => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home_dir_for(platform).map(|home| home.join(".config"))),
            HostPlatform::Windows => self
                .app_data
                .clone()
                .or_else(|| self.local_app_data.clone())
                .or_else(|| {
                    self.home_dir_for(platform)
                        .map(|home| home.join("AppData").join("Roaming"))
                }),
            HostPlatform::Other => self
                .xdg_config_home
                .clone()
                .or_else(|| self.home_dir_for(platform).map(|home| home.join(".config"))),
        }
    }

    /// Resolve the default MentisDB storage directory using the current
    /// environment snapshot.
    pub fn default_mentisdb_dir(&self) -> PathBuf {
        if let Some(path) = self
            .mentisdb_dir_override
            .clone()
            .filter(|path| !path.as_os_str().is_empty())
        {
            return path;
        }

        if let Some(home) = self
            .home_dir
            .clone()
            .or_else(|| self.user_profile.clone())
            .filter(|path| !path.as_os_str().is_empty())
        {
            return home.join(".cloudllm").join(MENTISDB_DIRNAME);
        }

        self.current_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".cloudllm")
            .join(MENTISDB_DIRNAME)
    }
}

/// Resolve the default on-disk MentisDB storage directory for the current
/// process environment.
///
/// This follows the same priority chain as the daemon:
///
/// 1. `MENTISDB_DIR`
/// 2. `$HOME/.cloudllm/mentisdb` (or `%USERPROFILE%\.cloudllm\mentisdb`)
/// 3. `./.cloudllm/mentisdb`
pub fn default_mentisdb_dir() -> PathBuf {
    PathEnvironment::capture().default_mentisdb_dir()
}

/// Path of the settings file the dashboard writes: `<MENTISDB_DIR>/.env`.
///
/// The daemon loads it at startup after any `.env` in the working
/// directory, so values saved from the dashboard Settings tab take effect on
/// restart regardless of where the daemon was launched from.
pub fn mentisdb_dir_env_file() -> PathBuf {
    default_mentisdb_dir().join(".env")
}

/// Load `.env` files into the process environment at daemon startup.
///
/// Order (earlier wins, since already-set variables are never overridden):
/// the real process environment, then `./.env` in the working directory, then
/// [`mentisdb_dir_env_file`]. The working-directory file is read first so it
/// may itself set `MENTISDB_DIR`. Missing files are ignored. Returns the files
/// that were loaded.
pub fn load_dotenv_files() -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    if let Ok(path) = dotenvy::dotenv() {
        loaded.push(path);
    }
    let dir_env = mentisdb_dir_env_file();
    if !loaded.contains(&dir_env) && dotenvy::from_path(&dir_env).is_ok() {
        loaded.push(dir_env);
    }
    loaded
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}
