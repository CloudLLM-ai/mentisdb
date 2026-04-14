//! Shared state for concrete integration apply plans.

use crate::integrations::files::ManagedFile;
use crate::integrations::IntegrationKind;
use crate::paths::HostPlatform;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const MCP_REMOTE_MIN_NODE_MAJOR: u32 = 20;

/// Resolved bridge command for Claude Desktop's mcp-remote transport.
///
/// Claude Desktop launches stdio-based MCP servers by running a `command` with
/// `args`.  When mcp-remote is installed via npm, its shebang is `#!/usr/bin/env node`
/// which may resolve to an older Node version (mcp-remote requires Node >= 20).
/// In that case we write the **absolute path to the `node` binary** as the command
/// and pass the **absolute path to the `mcp-remote` script** as the first argument.
///
/// When mcp-remote is installed via Homebrew, it has a proper shebang pointing
/// to the correct Node and is directly executable — no node wrapper needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BridgeCommand {
    /// Whether mcp-remote is directly executable (e.g. Homebrew-installed)
    /// and can be used as `command` without wrapping through `node`.
    pub(crate) directly_executable: bool,
    /// Absolute path to the `node` binary that satisfies mcp-remote's
    /// minimum Node version requirement (>= 20). Only meaningful when
    /// `directly_executable` is false.
    pub(crate) node_path: String,
    /// Absolute path to the `mcp-remote` script.
    pub(crate) mcp_remote_path: String,
}

/// Shared writer settings for MentisDB host integrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationWriterSettings {
    /// Stable MCP server name written into client config files.
    server_name: String,
    /// Default local streamable HTTP MCP URL used by most clients.
    default_http_url: String,
    /// Default HTTPS MCP URL used by bridge-based clients.
    default_https_url: String,
    /// Optional explicit bridge command (node + mcp-remote) for Claude Desktop.
    claude_desktop_bridge_command: Option<BridgeCommand>,
}

impl Default for IntegrationWriterSettings {
    fn default() -> Self {
        Self {
            server_name: "mentisdb".to_owned(),
            default_http_url: "http://127.0.0.1:9471".to_owned(),
            default_https_url: "https://my.mentisdb.com:9473".to_owned(),
            claude_desktop_bridge_command: None,
        }
    }
}

impl IntegrationWriterSettings {
    pub(crate) fn with_url_for(
        &self,
        integration: IntegrationKind,
        url: impl Into<String>,
    ) -> Self {
        let mut next = self.clone();
        let url = url.into();
        match integration {
            IntegrationKind::ClaudeDesktop => next.default_https_url = url,
            _ => next.default_http_url = url,
        }
        next
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.server_name
    }

    pub(crate) fn url_for(&self, integration: IntegrationKind) -> &str {
        match integration {
            IntegrationKind::ClaudeDesktop => &self.default_https_url,
            _ => &self.default_http_url,
        }
    }

    pub(crate) fn bridge_command_for(&self, platform: HostPlatform) -> BridgeCommand {
        self.claude_desktop_bridge_command
            .clone()
            .unwrap_or_else(|| detect_bridge_command(platform))
    }
}

/// Fully expanded file-write plan for one integration target.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IntegrationApplyPlan {
    /// Integration that will be updated.
    pub(crate) integration: IntegrationKind,
    /// Platform-specific path mapping used for the plan.
    pub(crate) platform: HostPlatform,
    /// Managed files to merge or create.
    pub(crate) files: Vec<ManagedFile>,
}

impl IntegrationApplyPlan {
    pub(crate) fn new(integration: IntegrationKind, platform: HostPlatform) -> Self {
        Self {
            integration,
            platform,
            files: Vec::new(),
        }
    }

    pub(crate) fn with_file(mut self, file: ManagedFile) -> Self {
        self.files.push(file);
        self
    }
}

fn detect_bridge_command(platform: HostPlatform) -> BridgeCommand {
    let mcp_remote_path = detect_mcp_remote_path(platform);
    // A binary found at an absolute path is always treated as directly executable.
    // The node-wrapper path is only needed for npm-installed mcp-remote scripts
    // with a `#!/usr/bin/env node` shebang pointing to an old Node version.
    // Homebrew-installed binaries are compiled wrappers, not text scripts, so
    // the shebang check is both unreliable and unnecessary for them.
    let directly_executable = is_absolute_executable(&mcp_remote_path)
        || is_directly_executable_via_shebang(&mcp_remote_path);
    let node_path = if directly_executable {
        String::new()
    } else {
        detect_node_path(&mcp_remote_path, platform)
    };
    BridgeCommand {
        directly_executable,
        node_path,
        mcp_remote_path,
    }
}

/// Returns true if the path is absolute and points to an existing executable file.
/// Used for Homebrew-installed binaries that are compiled wrappers, not scripts.
fn is_absolute_executable(path_str: &str) -> bool {
    let path = Path::new(path_str);
    path.is_absolute() && is_executable_file(path)
}

fn detect_mcp_remote_path(platform: HostPlatform) -> String {
    let binary_name = match platform {
        HostPlatform::Windows => "mcp-remote.cmd",
        _ => "mcp-remote",
    };

    for entry in split_path_entries(env::var_os("PATH")) {
        let candidate = entry.join(binary_name);
        if is_executable_file(&candidate) {
            return candidate.display().to_string();
        }
    }

    if matches!(platform, HostPlatform::Macos) {
        for candidate in [
            PathBuf::from("/opt/homebrew/bin/mcp-remote"),
            PathBuf::from("/usr/local/bin/mcp-remote"),
        ] {
            if is_executable_file(&candidate) {
                return candidate.display().to_string();
            }
        }
    }

    binary_name.to_owned()
}

/// Detect the `node` binary that should run mcp-remote.
///
/// Strategy:
/// 1. If the mcp-remote script was found on PATH, look for `node` in the
///    **same directory** first (e.g. an nvm-managed bin dir where both live).
/// 2. Fall back to `node` anywhere on PATH.
/// 3. Fall back to common macOS Homebrew paths.
/// 4. Last resort: bare `node`.
fn detect_node_path(mcp_remote_path: &str, platform: HostPlatform) -> String {
    if let Some(parent) = Path::new(mcp_remote_path).parent() {
        let candidate = parent.join("node");
        if is_executable_file(&candidate) {
            return candidate.display().to_string();
        }
    }

    for entry in split_path_entries(env::var_os("PATH")) {
        let candidate = entry.join("node");
        if is_executable_file(&candidate) {
            return candidate.display().to_string();
        }
    }

    if matches!(platform, HostPlatform::Macos) {
        for candidate in [
            PathBuf::from("/opt/homebrew/bin/node"),
            PathBuf::from("/usr/local/bin/node"),
        ] {
            if is_executable_file(&candidate) {
                return candidate.display().to_string();
            }
        }
    }

    "node".to_owned()
}

fn split_path_entries(path: Option<OsString>) -> Vec<PathBuf> {
    path.map(|value| env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }
}

/// Check whether mcp-remote has a valid shebang pointing to an existing node binary.
///
/// This is a fallback for npm-installed mcp-remote scripts: if the shebang points to
/// an absolute node path with version >= 20, we can run the script directly.
/// For all absolute-path executables, [`is_absolute_executable`] is preferred.
fn is_directly_executable_via_shebang(mcp_remote_path: &str) -> bool {
    let path = Path::new(mcp_remote_path);
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let shebang_line = match content.strip_prefix("#!") {
        Some(s) => s.split('\n').next(),
        None => return false,
    };
    let node_in_shebang = match shebang_line {
        Some(s) => s.split_whitespace().next(),
        None => return false,
    };
    let node_path = match node_in_shebang {
        Some(p) => PathBuf::from(p),
        None => return false,
    };
    if !node_path.is_absolute() || !is_executable_file(&node_path) {
        return false;
    }

    if let Ok(major) = node_major_version_from_path(&node_path) {
        return major >= MCP_REMOTE_MIN_NODE_MAJOR;
    }

    false
}

fn node_major_version_from_path(node_path: &Path) -> io::Result<u32> {
    let output = Command::new(node_path).arg("--version").output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "node --version exited with status {}",
            output.status
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_node_major(&version)
}

pub fn parse_node_major(version: &str) -> io::Result<u32> {
    let version = version.trim_start_matches('v');
    let major_str = version
        .split('.')
        .next()
        .ok_or_else(|| io::Error::other(format!("unexpected node version format: {version}")))?;
    major_str.parse::<u32>().map_err(|e| {
        io::Error::other(format!(
            "could not parse node major version from {version}: {e}"
        ))
    })
}
