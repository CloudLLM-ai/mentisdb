//! Shared setup-plan generation for host integrations.

use crate::integrations::detect::{
    detect_integrations, detect_integrations_with_environment, DetectionReport, DetectionStatus,
    IntegrationDetection,
};
use crate::integrations::{
    IntegrationFileFormat, IntegrationKind, IntegrationPathKind, IntegrationPathTarget,
    IntegrationSpec,
};
use crate::paths::{HostPlatform, PathEnvironment};
use std::path::PathBuf;

/// Primary action the setup flow should take for an integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetupAction {
    /// Update an existing canonical config file.
    UpdateExistingConfig,
    /// Create the canonical config file at the standard path.
    CreateCanonicalConfig,
}

impl SetupAction {
    /// Return a stable lowercase identifier for display.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpdateExistingConfig => "update_existing_config",
            Self::CreateCanonicalConfig => "create_canonical_config",
        }
    }
}

/// Setup target with enough metadata for a future writer or UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupTarget {
    /// Path to the file or directory.
    pub path: PathBuf,
    /// Expected filesystem shape.
    pub kind: IntegrationPathKind,
    /// Short explanation of the target's role.
    pub purpose: &'static str,
    /// Optional content-format hint.
    pub format: Option<IntegrationFileFormat>,
    /// `true` if the target already exists with the expected kind.
    pub exists: bool,
    /// Parent directory that must exist before writing this target.
    pub parent_dir: Option<PathBuf>,
    /// `true` if the parent directory is currently missing.
    pub create_parent_dir: bool,
}

/// Renderable setup scaffold for one integration target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPlan {
    /// Target integration.
    pub integration: IntegrationKind,
    /// Host platform used for path resolution.
    pub platform: HostPlatform,
    /// Target MentisDB MCP endpoint URL.
    pub url: String,
    /// Shared filesystem spec for the target integration.
    pub spec: IntegrationSpec,
    /// Filesystem detection status for the target integration.
    pub detection_status: DetectionStatus,
    /// Recommended primary action for setup.
    pub action: SetupAction,
    /// Canonical config target plus any optional companion files.
    pub targets: Vec<SetupTarget>,
    /// Optional CLI command that can register the integration directly.
    pub suggested_command: Option<String>,
    /// Optional example config snippet.
    pub snippet: Option<String>,
    /// Additional operator notes.
    pub notes: Vec<String>,
}

/// Setup plan covering every supported integration on one platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCatalogPlan {
    /// Platform used for resolution.
    pub platform: HostPlatform,
    /// Environment snapshot used for planning.
    pub environment: PathEnvironment,
    /// Per-integration plans.
    pub integrations: Vec<SetupPlan>,
}

impl SetupCatalogPlan {
    /// Return the plan entry for one integration, if present.
    pub fn integration(&self, kind: IntegrationKind) -> Option<&SetupPlan> {
        self.integrations
            .iter()
            .find(|entry| entry.integration == kind)
    }
}

/// Build a shared setup plan for a supported integration and explicit URL.
pub fn build_setup_plan_for_integration(
    integration: IntegrationKind,
    url: impl Into<String>,
    platform: HostPlatform,
    env: &PathEnvironment,
) -> Option<SetupPlan> {
    let url = url.into();
    let report = detect_integrations_with_environment(platform, env.clone());
    let detection = report.integration(integration)?.clone();
    Some(plan_from_detection(detection, url))
}

/// Detect integrations on the current host and convert them into setup plans.
pub fn plan_detected_integrations() -> SetupCatalogPlan {
    build_detected_setup_catalog(detect_integrations())
}

/// Convert a detection report into a catalog of per-integration setup plans.
pub fn build_setup_plan(report: DetectionReport) -> SetupCatalogPlan {
    build_detected_setup_catalog(report)
}

/// Convert a detection report into a catalog of per-integration setup plans.
pub fn build_detected_setup_catalog(report: DetectionReport) -> SetupCatalogPlan {
    let integrations = report
        .integrations
        .into_iter()
        .map(|detection| {
            let url = default_url_for_integration(detection.integration).to_string();
            plan_from_detection(detection, url)
        })
        .collect();

    SetupCatalogPlan {
        platform: report.platform,
        environment: report.environment,
        integrations,
    }
}

fn plan_from_detection(detection: IntegrationDetection, url: String) -> SetupPlan {
    let integration = detection.integration;
    let platform = detection.platform;
    let spec = detection.spec.clone();
    let action = match detection.status {
        DetectionStatus::Configured => SetupAction::UpdateExistingConfig,
        DetectionStatus::InstalledOrUsed | DetectionStatus::NotDetected => {
            SetupAction::CreateCanonicalConfig
        }
    };
    let targets = collect_targets(&detection);
    let mut notes = spec.notes.clone();

    let suggested_command = match integration {
        IntegrationKind::Codex => Some(format!("codex mcp add mentisdb --url {url}")),
        IntegrationKind::ClaudeCode => {
            Some(format!("claude mcp add --transport http mentisdb {url}"))
        }
        IntegrationKind::Qwen => Some(format!("qwen mcp add --transport http mentisdb {url}")),
        IntegrationKind::CopilotCli => Some(format!(
            "Use `/mcp add` inside Copilot CLI or let `mentisdb setup copilot` write {}.",
            spec.config_target.path.display()
        )),
        _ => None,
    };

    let snippet = match integration {
        IntegrationKind::ClaudeCode => Some(format!(
            "{{\n  \"mcpServers\": {{\n    \"mentisdb\": {{\n      \"type\": \"http\",\n      \"url\": \"{url}\"\n    }}\n  }}\n}}"
        )),
        IntegrationKind::ClaudeDesktop => Some(
            "{\n  \"mcpServers\": {\n    \"mentisdb\": {\n      \"command\": \"mentisdb\",\n      \"args\": [\"--mode\", \"stdio\"]\n    }\n  }\n}\n// mentisdb communicates with Claude Desktop over stdio using the MCP protocol.".to_string()
        ),
        IntegrationKind::GeminiCli => Some(format!(
            "{{\n  \"mcpServers\": {{\n    \"mentisdb\": {{\n      \"type\": \"http\",\n      \"url\": \"{url}\",\n      \"httpUrl\": \"{url}\"\n    }}\n  }}\n}}"
        )),
        IntegrationKind::OpenCode => Some(format!(
            "{{\n  \"mcp\": {{\n    \"mentisdb\": {{\n      \"type\": \"remote\",\n      \"url\": \"{url}\",\n      \"enabled\": true\n    }}\n  }}\n}}"
        )),
        IntegrationKind::CopilotCli => Some(format!(
            "{{\n  \"mcpServers\": {{\n    \"mentisdb\": {{\n      \"type\": \"http\",\n      \"url\": \"{url}\",\n      \"headers\": {{}},\n      \"tools\": [\"*\"]\n    }}\n  }}\n}}"
        )),
        IntegrationKind::VsCodeCopilot => Some(format!(
            "{{\n  \"servers\": {{\n    \"mentisdb\": {{\n      \"type\": \"http\",\n      \"url\": \"{url}\"\n    }}\n  }}\n}}"
        )),
        _ => None,
    };

    match integration {
        IntegrationKind::ClaudeDesktop => {
            notes.push(
                "Claude Desktop spawns mentisdb as a subprocess and communicates over stdio using MCP JSON-RPC."
                    .to_string(),
            );
            notes.push(
                "Data is persisted to MENTISDB_DIR (default ~/.cloudllm/mentisdb) even when the subprocess exits."
                    .to_string(),
            );
            notes.push(
                "Make sure mentisdb is on your PATH, or use the full path to the binary."
                    .to_string(),
            );
        }
        IntegrationKind::GeminiCli => notes.push(
            "Gemini setup writes both 'url' and 'httpUrl' fields to cover current remote HTTP config variants."
                .to_string(),
        ),
        IntegrationKind::OpenCode => notes.push(
            "OpenCode uses the remote MCP block under the top-level 'mcp' key."
                .to_string(),
        ),
        IntegrationKind::CopilotCli => notes.push(
            format!(
                "Copilot CLI stores user MCP config in {}.",
                spec.config_target.path.display()
            ),
        ),
        _ => {}
    }
    notes.push(match detection.status {
        DetectionStatus::Configured => {
            "Canonical config file already exists; setup should merge or update in place."
                .to_string()
        }
        DetectionStatus::InstalledOrUsed => {
            "Host app looks present, but the canonical MentisDB config target is missing."
                .to_string()
        }
        DetectionStatus::NotDetected => {
            "No installation signal was found; setup may need to create the standard config path proactively."
                .to_string()
        }
    });

    SetupPlan {
        integration,
        platform,
        url,
        spec,
        detection_status: detection.status,
        action,
        targets,
        suggested_command,
        snippet,
        notes,
    }
}

fn collect_targets(detection: &IntegrationDetection) -> Vec<SetupTarget> {
    let mut targets = Vec::with_capacity(1 + detection.spec.companion_targets.len());
    let config_exists = detection
        .evidence
        .iter()
        .find(|item| item.path == detection.spec.config_target.path)
        .map(|item| item.exists)
        .unwrap_or(false);
    targets.push(target_from_path_target(
        &detection.spec.config_target,
        config_exists,
    ));
    for companion in &detection.spec.companion_targets {
        let exists = detection
            .evidence
            .iter()
            .find(|item| item.path == companion.path)
            .map(|item| item.exists)
            .unwrap_or(false);
        targets.push(target_from_path_target(companion, exists));
    }
    targets
}

fn target_from_path_target(target: &IntegrationPathTarget, exists: bool) -> SetupTarget {
    let parent_dir = target.path.parent().map(PathBuf::from);
    let create_parent_dir = parent_dir
        .as_ref()
        .map(|dir| !dir.exists())
        .unwrap_or(false);

    SetupTarget {
        path: target.path.clone(),
        kind: target.kind,
        purpose: target.purpose,
        format: target.format,
        exists,
        parent_dir,
        create_parent_dir,
    }
}

pub(crate) fn default_url_for_integration(integration: IntegrationKind) -> &'static str {
    match integration {
        IntegrationKind::ClaudeDesktop => "https://my.mentisdb.com:9473",
        _ => "http://127.0.0.1:9471",
    }
}
