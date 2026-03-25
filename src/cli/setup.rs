use crate::integrations::apply::apply_setup_with_environment;
use crate::integrations::plan::{build_setup_plan_for_integration, SetupPlan};
use crate::integrations::IntegrationKind;
use crate::paths::{HostPlatform, PathEnvironment};
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;

use super::args::default_url;
use super::SetupCommand;

pub(super) fn run_setup(command: &SetupCommand, out: &mut dyn Write) -> io::Result<()> {
    let env = PathEnvironment::capture();
    let platform = HostPlatform::current();

    for integration in &command.integrations {
        let url = command
            .url
            .clone()
            .unwrap_or_else(|| default_url(*integration).to_string());
        let Some(plan) =
            build_setup_plan_for_integration(*integration, url.clone(), platform, &env)
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported integration target",
            ));
        };

        write!(out, "{}", render_setup_plan(&plan))?;
        if command.dry_run {
            continue;
        }

        ensure_prerequisites(*integration, out)?;
        let result = apply_setup_with_environment(*integration, url, platform, &env)?;
        writeln!(
            out,
            "Applied: {} ({})\n",
            result.path.display(),
            if result.changed {
                "updated"
            } else {
                "unchanged"
            }
        )?;
    }

    Ok(())
}

/// Render a human-readable setup plan.
pub fn render_setup_plan(plan: &SetupPlan) -> String {
    let mut rendered = String::new();
    rendered.push_str("MentisDB setup plan\n\n");
    rendered.push_str(&format!(
        "Agent: {}\nPlatform: {}\nURL: {}\nTarget: {}\nStatus: {}\nAction: {}\n",
        plan.integration.display_name(),
        plan.platform.as_str(),
        plan.url,
        plan.spec.config_target.path.display(),
        plan.detection_status.as_str(),
        plan.action.as_str(),
    ));
    if let Some(command) = &plan.suggested_command {
        rendered.push_str(&format!("Command: {command}\n"));
    }
    if let Some(snippet) = &plan.snippet {
        rendered.push_str("\nExample config snippet:\n");
        rendered.push_str(snippet);
        rendered.push('\n');
    }
    if !plan.notes.is_empty() {
        rendered.push_str("\nNotes:\n");
        for note in &plan.notes {
            rendered.push_str("- ");
            rendered.push_str(note);
            rendered.push('\n');
        }
    }
    rendered.push('\n');
    rendered
}

pub(super) fn ensure_prerequisites(
    integration: IntegrationKind,
    out: &mut dyn Write,
) -> io::Result<()> {
    if integration != IntegrationKind::ClaudeDesktop {
        return Ok(());
    }
    if command_on_path(&["mcp-remote", "mcp-remote.cmd"]).is_some() {
        return Ok(());
    }

    let Some(npm) = command_on_path(&["npm", "npm.cmd"]) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Claude Desktop integration requires npm so MentisDB can install mcp-remote.",
        ));
    };

    writeln!(
        out,
        "Claude Desktop requires mcp-remote. Installing it with {}...",
        npm.display()
    )?;
    let status = Command::new(&npm)
        .args(["install", "-g", "mcp-remote"])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "npm install -g mcp-remote failed with status {status}"
        )));
    }
    Ok(())
}

fn command_on_path(candidates: &[&str]) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        for candidate in candidates {
            let path = dir.join(candidate);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}
