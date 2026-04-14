use crate::integrations::plan::{build_setup_plan_for_integration, SetupPlan};
use crate::integrations::IntegrationKind;
use crate::paths::{HostPlatform, PathEnvironment};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

use super::args::default_url;
use super::prompt::boxed_apply_summary;
use super::SetupCommand;

/// Minimum Node.js major version required by mcp-remote.
const MCP_REMOTE_MIN_NODE_MAJOR: u32 = 20;

pub(super) fn run_setup(
    command: &SetupCommand,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> io::Result<()> {
    let env = PathEnvironment::capture();
    let platform = HostPlatform::current();
    let mut plans = Vec::with_capacity(command.integrations.len());

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
        plans.push(plan);
    }

    for plan in &plans {
        write!(out, "{}", render_setup_plan(plan))?;
    }

    if command.dry_run {
        return Ok(());
    }

    let apply_items: Vec<(String, String)> = plans
        .iter()
        .map(|plan| {
            (
                plan.integration.display_name().to_owned(),
                plan.spec.config_target.path.display().to_string(),
            )
        })
        .collect();

    if !command.assume_yes {
        let response = boxed_apply_summary(out, &apply_items, true, input)?;
        if response.eq_ignore_ascii_case("n") || response.eq_ignore_ascii_case("no") {
            writeln!(out, "\nCancelled.")?;
            return Ok(());
        }
    }

    writeln!(out)?;
    let mut had_errors = false;
    for plan in plans {
        match ensure_prerequisites(plan.integration, command.assume_yes, out, input) {
            Ok(PrerequisiteStatus::Ok) | Ok(PrerequisiteStatus::Warning(_)) => {}
            Ok(PrerequisiteStatus::Skipped) => {
                writeln!(
                    out,
                    "Skipping {} — mcp-remote installation declined.",
                    plan.integration.display_name()
                )?;
                continue;
            }
            Err(e) => {
                writeln!(
                    out,
                    "Skipping {} — prerequisite check failed: {e}",
                    plan.integration.display_name()
                )?;
                had_errors = true;
                continue;
            }
        }
        match crate::integrations::apply::apply_setup_with_environment(
            plan.integration,
            plan.url.clone(),
            platform,
            &env,
        ) {
            Ok(result) => {
                writeln!(
                    out,
                    "{} -> {} ({})",
                    plan.integration.display_name(),
                    result.path.display(),
                    if result.changed {
                        "updated"
                    } else {
                        "unchanged"
                    }
                )?;
            }
            Err(e) => {
                writeln!(
                    out,
                    "Skipping {} — apply failed: {e}",
                    plan.integration.display_name()
                )?;
                had_errors = true;
            }
        }
    }

    if had_errors {
        writeln!(
            out,
            "\nSome integrations could not be configured. See warnings above."
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

/// Prerequisite check result for an integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrerequisiteStatus {
    /// All prerequisites met; proceed with apply.
    Ok,
    /// Prerequisites not met but setup can still write the config with a warning.
    /// The user can fix the issue later without re-running the wizard.
    Warning(String),
    /// User was asked whether to install mcp-remote and declined.
    Skipped,
}

pub(super) fn ensure_prerequisites(
    integration: IntegrationKind,
    assume_yes: bool,
    out: &mut dyn Write,
    input: &mut dyn BufRead,
) -> io::Result<PrerequisiteStatus> {
    use super::prompt::boxed_yn_prompt;

    if integration != IntegrationKind::ClaudeDesktop {
        return Ok(PrerequisiteStatus::Ok);
    }

    // mcp-remote already present — nothing to install
    if detect_mcp_remote_any().is_some() {
        return Ok(PrerequisiteStatus::Ok);
    }

    let platform = HostPlatform::current();

    // ── macOS: prefer Homebrew ────────────────────────────────────────────────
    if matches!(platform, HostPlatform::Macos) {
        if let Some(brew_path) = detect_brew() {
            if !assume_yes {
                let answer = boxed_yn_prompt(
                    out,
                    "mcp-remote is not installed. Install it via Homebrew (recommended)?",
                    true,
                    input,
                )?;
                if answer.eq_ignore_ascii_case("n") || answer.eq_ignore_ascii_case("no") {
                    // Fall through to npm
                } else {
                    writeln!(out, "Installing mcp-remote via Homebrew...")?;
                    let status = Command::new(&brew_path)
                        .args(["install", "mcp-remote"])
                        .status()?;
                    if status.success() {
                        writeln!(out, "mcp-remote installed via Homebrew.")?;
                        return Ok(PrerequisiteStatus::Ok);
                    }
                    let msg = "brew install mcp-remote failed. Will try npm.".to_string();
                    writeln!(out, "WARNING: {msg}")?;
                }
            } else {
                // assume_yes: install via brew silently
                writeln!(out, "Installing mcp-remote via Homebrew...")?;
                let status = Command::new(&brew_path)
                    .args(["install", "mcp-remote"])
                    .status()?;
                if status.success() {
                    writeln!(out, "mcp-remote installed via Homebrew.")?;
                    return Ok(PrerequisiteStatus::Ok);
                }
                let msg = "brew install mcp-remote failed. Will try npm.".to_string();
                writeln!(out, "WARNING: {msg}")?;
            }
        }
    }

    // ── Fallback: npm ─────────────────────────────────────────────────────────
    let Some(npm) = command_on_path(&["npm", "npm.cmd"]) else {
        let msg = "mcp-remote is not installed and neither Homebrew nor npm is available.\nClaude Desktop will not connect until mcp-remote is installed.\nInstall manually: brew install mcp-remote".to_string();
        writeln!(out, "WARNING: {msg}")?;
        return Ok(PrerequisiteStatus::Warning(msg));
    };

    // Check Node version before npm-installing
    if let Some(node) = command_on_path(&["node", "node.exe"]) {
        match node_major_version(&node) {
            Ok(major) if major < MCP_REMOTE_MIN_NODE_MAJOR => {
                let msg = format!(
                    "mcp-remote requires Node.js >= {MCP_REMOTE_MIN_NODE_MAJOR}, but {} is Node {major}.\nThe config will be written but Claude Desktop will not connect until you install Node >= {MCP_REMOTE_MIN_NODE_MAJOR}.",
                    node.display()
                );
                writeln!(out, "WARNING: {msg}")?;
                return Ok(PrerequisiteStatus::Warning(msg));
            }
            Ok(_) => {}
            Err(e) => {
                writeln!(out, "WARNING: Could not determine Node.js version: {e}.")?;
            }
        }
    } else {
        let msg = "Node.js is not on PATH. Claude Desktop will not connect until Node.js >= 20 and mcp-remote are installed.".to_string();
        writeln!(out, "WARNING: {msg}")?;
        return Ok(PrerequisiteStatus::Warning(msg));
    }

    if !assume_yes {
        let answer = boxed_yn_prompt(
            out,
            "mcp-remote is not installed. Install it via npm?",
            false,
            input,
        )?;
        if !answer.eq_ignore_ascii_case("y") && !answer.eq_ignore_ascii_case("yes") {
            return Ok(PrerequisiteStatus::Skipped);
        }
    }

    writeln!(out, "Installing mcp-remote via npm...")?;
    let status = Command::new(&npm)
        .args(["install", "-g", "mcp-remote"])
        .status()?;
    if !status.success() {
        let msg = "npm install -g mcp-remote failed.".to_string();
        writeln!(out, "WARNING: {msg}")?;
        return Ok(PrerequisiteStatus::Warning(msg));
    }

    writeln!(out, "mcp-remote installed via npm.")?;
    Ok(PrerequisiteStatus::Ok)
}

/// Returns the path to mcp-remote if it is available by any means.
fn detect_mcp_remote_any() -> Option<PathBuf> {
    // Check known Homebrew locations first (highest priority on macOS)
    for candidate in [
        PathBuf::from("/opt/homebrew/bin/mcp-remote"),
        PathBuf::from("/usr/local/bin/mcp-remote"),
    ] {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Then PATH (covers npm global installs, nvm, etc.)
    command_on_path(&["mcp-remote", "mcp-remote.cmd"])
}

fn detect_brew() -> Option<PathBuf> {
    command_on_path(&["brew", "brew.cmd"])
}

fn node_major_version(node: &PathBuf) -> io::Result<u32> {
    let output = Command::new(node).arg("--version").output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "node --version exited with status {}",
            output.status
        )));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_node_major(&version)
}

/// Parse the major version number from a Node.js version string like `v22.18.0`.
pub fn parse_node_major(version: &str) -> io::Result<u32> {
    let version = version.trim_start_matches('v');
    let major_str = version
        .split('.')
        .next()
        .ok_or_else(|| io::Error::other(format!("unexpected node version format: {version}")))?;
    major_str.parse::<u32>().map_err(|e| {
        io::Error::other(format!(
            "could not parse node major version from {version}: {e}"
        ))
    })
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

// kept for potential future use
#[allow(dead_code)]
fn detect_brew_mcp_remote() -> Option<PathBuf> {
    [
        PathBuf::from("/opt/homebrew/bin/mcp-remote"),
        PathBuf::from("/usr/local/bin/mcp-remote"),
    ]
    .into_iter()
    .find(|candidate| candidate.exists())
}
