use crate::integrations::files::{JsonPatch, ManagedFile};
use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};
use serde_json::json;

pub(super) fn build(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    let mut patch = JsonPatch::new()
        .set_path(
            ["mcpServers", settings.server_name(), "type"],
            json!("http"),
        )
        .set_path(
            ["mcpServers", settings.server_name(), "url"],
            json!(settings.url_for(plan.integration)),
        );
    if let Some(token) = settings.bearer_token() {
        patch = patch.set_path(
            [
                "mcpServers",
                settings.server_name(),
                "headers",
                "Authorization",
            ],
            json!(format!("Bearer {}", token)),
        );
    }

    IntegrationApplyPlan::new(plan.integration, plan.platform).with_file(ManagedFile::json(
        plan.spec.config_target.path.clone(),
        patch,
    ))
}
