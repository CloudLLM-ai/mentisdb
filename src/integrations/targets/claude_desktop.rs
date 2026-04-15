use crate::integrations::files::{JsonPatch, ManagedFile};
use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};
use serde_json::json;

pub(super) fn build(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    let mut patch = JsonPatch::new();

    let mentisdbd_path = detect_mentisdbd_path();

    patch = patch.set_path(
        ["mcpServers", settings.server_name(), "command"],
        json!(mentisdbd_path),
    );

    patch = patch.set_path(
        ["mcpServers", settings.server_name(), "args"],
        json!(["--mode", "stdio"]),
    );

    IntegrationApplyPlan::new(plan.integration, plan.platform).with_file(ManagedFile::json(
        plan.spec.config_target.path.clone(),
        patch,
    ))
}

fn detect_mentisdbd_path() -> String {
    let binary_name = if cfg!(target_os = "windows") {
        "mentisdbd.exe"
    } else {
        "mentisdbd"
    };

    for entry in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = entry.join(binary_name);
        if candidate.is_file() {
            if cfg!(unix) {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(&candidate) {
                    if metadata.permissions().mode() & 0o111 != 0 {
                        return candidate.display().to_string();
                    }
                }
            } else {
                return candidate.display().to_string();
            }
        }
    }

    binary_name.to_string()
}
