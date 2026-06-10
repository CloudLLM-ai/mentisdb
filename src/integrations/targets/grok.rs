use crate::integrations::files::{ManagedFile, TomlPatch, TomlValue};
use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};

pub(super) fn build(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    let mut patch = TomlPatch::new().set_path(
        ["mcp_servers", settings.server_name(), "url"],
        TomlValue::from(settings.url_for(plan.integration).to_owned()),
    );
    if let Some(token) = settings.bearer_token() {
        patch = patch.set_path(
            ["mcp_servers", settings.server_name(), "http_headers", "Authorization"],
            TomlValue::from(format!("Bearer {}", token)),
        );
    }

    IntegrationApplyPlan::new(plan.integration, plan.platform).with_file(ManagedFile::toml(
        plan.spec.config_target.path.clone(),
        patch,
    ))
}
