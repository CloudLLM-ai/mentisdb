use crate::integrations::files::{JsonPatch, ManagedFile};
use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};
use serde_json::json;

pub(super) fn build(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    let url = settings.url_for(plan.integration);
    let bridge = settings.bridge_command_for(plan.platform);
    let mut patch = JsonPatch::new();

    if bridge.directly_executable {
        patch = patch.set_path(
            ["mcpServers", settings.server_name(), "command"],
            json!(bridge.mcp_remote_path),
        );
    } else {
        patch = patch.set_path(
            ["mcpServers", settings.server_name(), "command"],
            json!(bridge.node_path),
        );
    }

    if bridge.directly_executable {
        patch = patch.set_path(["mcpServers", settings.server_name(), "args"], json!([url]));
    } else {
        patch = patch.set_path(
            ["mcpServers", settings.server_name(), "args"],
            json!([bridge.mcp_remote_path, url]),
        );
    }

    // SECURITY: NODE_TLS_REJECT_UNAUTHORIZED=0 disables TLS certificate verification for the
    // mcp-remote bridge. This is required for self-signed certificates on private servers.
    // A visible warning is surfaced via the SetupPlan notes when building the plan in plan.rs.
    // TODO: expose a user-facing --allow-insecure flag to make this opt-in rather than automatic.
    if url.starts_with("https://") {
        patch = patch.set_path(
            [
                "mcpServers",
                settings.server_name(),
                "env",
                "NODE_TLS_REJECT_UNAUTHORIZED",
            ],
            json!("0"),
        );
    }

    IntegrationApplyPlan::new(plan.integration, plan.platform).with_file(ManagedFile::json(
        plan.spec.config_target.path.clone(),
        patch,
    ))
}
