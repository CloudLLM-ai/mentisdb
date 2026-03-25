//! Filesystem detection helpers for MentisDB host integrations.

use crate::integrations::{
    integration_specs, IntegrationKind, IntegrationPathKind, IntegrationSpec,
};
use crate::paths::{HostPlatform, PathEnvironment};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use toml_edit::DocumentMut;

/// High-level detection result for one integration target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectionStatus {
    /// The canonical configuration file already exists.
    Configured,
    /// A probe path exists, indicating the host is installed or has been used.
    InstalledOrUsed,
    /// No installation or usage signal was found.
    NotDetected,
}

impl DetectionStatus {
    /// Return a stable lowercase identifier for display.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::InstalledOrUsed => "installed_or_used",
            Self::NotDetected => "not_detected",
        }
    }
}

/// One filesystem observation collected during detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionEvidence {
    /// Probed path.
    pub path: PathBuf,
    /// Whether the path exists with the expected type.
    pub exists: bool,
}

/// Detection output for one integration target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDetection {
    /// Integration that was checked.
    pub integration: IntegrationKind,
    /// Platform used to resolve this integration's paths.
    pub platform: HostPlatform,
    /// Shared integration spec used for detection.
    pub spec: IntegrationSpec,
    /// High-level detection status.
    pub status: DetectionStatus,
    /// Filesystem evidence gathered for config, probes, and companion paths.
    pub evidence: Vec<DetectionEvidence>,
}

/// Aggregated detection output for one platform and environment snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionReport {
    /// Platform that was scanned.
    pub platform: HostPlatform,
    /// Environment snapshot used during path resolution.
    pub environment: PathEnvironment,
    /// Per-integration detection results.
    pub integrations: Vec<IntegrationDetection>,
}

impl DetectionReport {
    /// Return the detection result for one integration, if present.
    pub fn integration(&self, integration: IntegrationKind) -> Option<&IntegrationDetection> {
        self.integrations
            .iter()
            .find(|entry| entry.integration == integration)
    }
}

/// Detect integrations for the current process environment.
pub fn detect_integrations() -> DetectionReport {
    let platform = HostPlatform::current();
    let env = PathEnvironment::capture();
    detect_integrations_with_environment(platform, env)
}

/// Detect integrations for an explicit platform and environment snapshot.
pub fn detect_integrations_with_environment(
    platform: HostPlatform,
    env: PathEnvironment,
) -> DetectionReport {
    let integrations = integration_specs(platform, &env)
        .into_iter()
        .map(|spec| detect_integration(platform, spec))
        .collect();

    DetectionReport {
        platform,
        environment: env,
        integrations,
    }
}

fn detect_integration(platform: HostPlatform, spec: IntegrationSpec) -> IntegrationDetection {
    let mut evidence =
        Vec::with_capacity(1 + spec.detection_probes.len() + spec.companion_targets.len());

    let config_exists = target_exists(&spec.config_target);
    let configured = config_contains_mentisdb_entry(&spec);
    evidence.push(DetectionEvidence {
        path: spec.config_target.path.clone(),
        exists: config_exists,
    });

    for probe in &spec.detection_probes {
        evidence.push(DetectionEvidence {
            path: probe.path.clone(),
            exists: target_exists(probe),
        });
    }

    for companion in &spec.companion_targets {
        evidence.push(DetectionEvidence {
            path: companion.path.clone(),
            exists: target_exists(companion),
        });
    }

    let status = if configured {
        DetectionStatus::Configured
    } else if spec.detection_probes.iter().any(target_exists) {
        DetectionStatus::InstalledOrUsed
    } else {
        DetectionStatus::NotDetected
    };

    IntegrationDetection {
        integration: spec.integration,
        platform,
        spec,
        status,
        evidence,
    }
}

fn target_exists(target: &crate::integrations::IntegrationPathTarget) -> bool {
    match target.kind {
        IntegrationPathKind::File => target.path.is_file(),
        IntegrationPathKind::Directory => target.path.is_dir(),
    }
}

fn config_contains_mentisdb_entry(spec: &IntegrationSpec) -> bool {
    let path = &spec.config_target.path;
    if !path.exists() {
        return false;
    }

    match spec.integration {
        IntegrationKind::Codex => toml_has_entry(path, &["mcp_servers", "mentisdb"]),
        IntegrationKind::ClaudeCode => path.is_file(),
        IntegrationKind::GeminiCli
        | IntegrationKind::Qwen
        | IntegrationKind::CopilotCli
        | IntegrationKind::ClaudeDesktop => json_has_entry(path, &["mcpServers", "mentisdb"]),
        IntegrationKind::OpenCode => json_has_entry(path, &["mcp", "mentisdb"]),
        IntegrationKind::VsCodeCopilot => json_has_entry(path, &["servers", "mentisdb"]),
    }
}

fn toml_has_entry(path: &std::path::Path, keys: &[&str]) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(document) = content.parse::<DocumentMut>() else {
        return false;
    };

    let mut current = document.as_item();
    for key in keys {
        let Some(next) = current.get(key) else {
            return false;
        };
        current = next;
    }
    !current.is_none()
}

fn json_has_entry(path: &std::path::Path, keys: &[&str]) -> bool {
    let Ok(content) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let mut current = &value;
    for key in keys {
        let Some(next) = current.get(*key) else {
            return false;
        };
        current = next;
    }
    !current.is_null()
}
