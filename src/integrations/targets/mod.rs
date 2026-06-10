use crate::integrations::plan::SetupPlan;
use crate::integrations::state::{IntegrationApplyPlan, IntegrationWriterSettings};
use crate::integrations::IntegrationKind;

mod claude_code;
mod claude_desktop;
mod codex;
mod copilot_cli;
mod gemini;
mod grok;
mod opencode;
mod qwen;
mod vscode_copilot;

pub(crate) fn build_apply_plan(
    plan: &SetupPlan,
    settings: &IntegrationWriterSettings,
) -> IntegrationApplyPlan {
    match plan.integration {
        IntegrationKind::Codex => codex::build(plan, settings),
        IntegrationKind::ClaudeCode => claude_code::build(plan, settings),
        IntegrationKind::GeminiCli => gemini::build(plan, settings),
        IntegrationKind::OpenCode => opencode::build(plan, settings),
        IntegrationKind::Qwen => qwen::build(plan, settings),
        IntegrationKind::CopilotCli => copilot_cli::build(plan, settings),
        IntegrationKind::VsCodeCopilot => vscode_copilot::build(plan, settings),
        IntegrationKind::ClaudeDesktop => claude_desktop::build(plan, settings),
        IntegrationKind::Grok => grok::build(plan, settings),
    }
}
