use crate::integrations::files::{ManagedFile, TomlPatch, TomlValue};
use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};

pub(super) fn build(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    let mut patch = TomlPatch::new()
        .set_path(
            ["mcp_servers", settings.server_name(), "url"],
            TomlValue::from(settings.url_for(plan.integration).to_owned()),
        )
        .set_path(
            ["mcp_servers", settings.server_name(), "enabled"],
            TomlValue::from(true),
        );
    if let Some(token) = settings.bearer_token() {
        patch = patch.set_path(
            ["mcp_servers", settings.server_name(), "headers", "Authorization"],
            TomlValue::from(format!("Bearer {}", token)),
        );
        // Only needed for https + self-signed certs. Plain http does not need it.
        if settings.url_for(plan.integration).starts_with("https://") {
            patch = patch.set_path(
                ["mcp_servers", settings.server_name(), "env", "NODE_TLS_REJECT_UNAUTHORIZED"],
                TomlValue::from("0"),
            );
        }
    }

    IntegrationApplyPlan::new(plan.integration, plan.platform).with_file(ManagedFile::toml(
        plan.spec.config_target.path.clone(),
        patch,
    ))
}
