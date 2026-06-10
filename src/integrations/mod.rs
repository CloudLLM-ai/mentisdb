//! Host-agent integration models for MentisDB setup, detection, and writes.
//!
//! The path-spec and detection types are reusable from the CLI wizard,
//! dashboard diagnostics, or any other surface that needs to reason about
//! host-agent setup state. The internal `files`, `state`, and `targets`
//! modules add the idempotent config-writer layer on top of that shared
//! catalog.

pub mod apply;
pub mod detect;
mod files;
pub mod plan;
pub mod platform;
mod state;
mod targets;

use crate::paths::{HostPlatform, PathEnvironment};
use std::fmt;
use std::path::PathBuf;

/// Supported host integrations that MentisDB can configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntegrationKind {
    /// OpenAI Codex CLI.
    Codex,
    /// Claude Code CLI.
    ClaudeCode,
    /// Gemini CLI.
    GeminiCli,
    /// OpenCode.
    OpenCode,
    /// Qwen CLI/code assistant.
    Qwen,
    /// GitHub Copilot CLI.
    CopilotCli,
    /// VS Code with Copilot and MCP support.
    VsCodeCopilot,
    /// Claude Desktop app.
    ClaudeDesktop,
    /// Grok CLI / Build TUI.
    Grok,
}

impl IntegrationKind {
    /// Return every known integration in stable display order.
    pub const ALL: [Self; 9] = [
        Self::Codex,
        Self::ClaudeCode,
        Self::GeminiCli,
        Self::OpenCode,
        Self::Qwen,
        Self::CopilotCli,
        Self::VsCodeCopilot,
        Self::ClaudeDesktop,
        Self::Grok,
    ];

    /// Return a stable lowercase identifier for this integration.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
            Self::GeminiCli => "gemini",
            Self::OpenCode => "opencode",
            Self::Qwen => "qwen",
            Self::CopilotCli => "copilot",
            Self::VsCodeCopilot => "vscode-copilot",
            Self::ClaudeDesktop => "claude-desktop",
            Self::Grok => "grok",
        }
    }

    /// Return a human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
            Self::GeminiCli => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::Qwen => "Qwen",
            Self::CopilotCli => "GitHub Copilot CLI",
            Self::VsCodeCopilot => "VS Code / Copilot",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Grok => "Grok CLI",
        }
    }
}

impl fmt::Display for IntegrationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

/// Filesystem shape expected for a target or detection probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegrationPathKind {
    /// Path should point at a regular file.
    File,
    /// Path should point at a directory.
    Directory,
}

/// Supported file format for a setup target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntegrationFileFormat {
    /// TOML configuration.
    Toml,
    /// JSON configuration.
    Json,
    /// Markdown content.
    Markdown,
}

impl IntegrationFileFormat {
    /// Return the lowercase format name for display.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Toml => "toml",
            Self::Json => "json",
            Self::Markdown => "markdown",
        }
    }
}

/// A file or directory MentisDB may need to inspect or write for an
/// integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPathTarget {
    /// Absolute or canonical user-scoped path.
    pub path: PathBuf,
    /// Whether the path is expected to be a file or a directory.
    pub kind: IntegrationPathKind,
    /// Short explanation of what the path is used for.
    pub purpose: &'static str,
    /// Optional content-format hint for file targets.
    pub format: Option<IntegrationFileFormat>,
}

impl IntegrationPathTarget {
    /// Build a file target.
    pub fn file(path: PathBuf, purpose: &'static str, format: IntegrationFileFormat) -> Self {
        Self {
            path,
            kind: IntegrationPathKind::File,
            purpose,
            format: Some(format),
        }
    }

    /// Build a directory target.
    pub fn directory(path: PathBuf, purpose: &'static str) -> Self {
        Self {
            path,
            kind: IntegrationPathKind::Directory,
            purpose,
            format: None,
        }
    }
}

/// Platform-specific file targets and notes for one integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationSpec {
    /// The integration being described.
    pub integration: IntegrationKind,
    /// The platform these paths apply to.
    pub platform: HostPlatform,
    /// Primary configuration file to read or write.
    pub config_target: IntegrationPathTarget,
    /// Additional directories or files that indicate local installation/use.
    pub detection_probes: Vec<IntegrationPathTarget>,
    /// Companion targets the future setup flow may want to create or inspect.
    pub companion_targets: Vec<IntegrationPathTarget>,
    /// Free-form notes and caveats for the planner/UI.
    pub notes: Vec<String>,
}

impl IntegrationSpec {
    /// Return the parent directory of the primary config target.
    pub fn config_parent_dir(&self) -> Option<PathBuf> {
        self.config_target.path.parent().map(PathBuf::from)
    }
}

/// Return the integration path catalog for a platform and environment snapshot.
pub fn integration_specs(platform: HostPlatform, env: &PathEnvironment) -> Vec<IntegrationSpec> {
    platform::specs_for(platform, env)
}
