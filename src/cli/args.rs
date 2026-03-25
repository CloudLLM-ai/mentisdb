use crate::integrations::plan::default_url_for_integration;
use crate::integrations::IntegrationKind;
use std::ffi::OsString;

/// Parsed `setup` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCommand {
    /// Target agent integrations to configure.
    pub integrations: Vec<IntegrationKind>,
    /// Optional target MentisDB MCP endpoint URL override.
    pub url: Option<String>,
    /// Render plans but do not write files.
    pub dry_run: bool,
}

/// Parsed `wizard` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardCommand {
    /// Optional preselected MentisDB MCP endpoint URL.
    pub url: Option<String>,
    /// Apply the default detected selection without prompting for confirmation.
    pub assume_yes: bool,
}

/// Supported top-level commands for `mentisdbd` setup and wizard subcommands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Print CLI help.
    Help,
    /// Print a setup scaffold for one target agent.
    Setup(SetupCommand),
    /// Run the interactive setup wizard.
    Wizard(WizardCommand),
}

/// Parse command-line arguments for the embedded `mentisdbd` setup and wizard CLI.
pub fn parse_args<I, T>(args: I) -> Result<CliCommand, String>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut parts = args
        .into_iter()
        .map(|arg| arg.into().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Ok(CliCommand::Help);
    }
    parts.remove(0);

    let Some(subcommand) = parts.first() else {
        return Ok(CliCommand::Help);
    };

    match subcommand.as_str() {
        "-h" | "--help" | "help" => Ok(CliCommand::Help),
        "setup" => parse_setup(parts),
        "wizard" => parse_wizard(parts),
        other => Err(format!("Unknown subcommand '{other}'")),
    }
}

pub(crate) fn help_text() -> &'static str {
    "\
mentisdbd setup and wizard

Usage:
  mentisdbd --help
  mentisdbd
  mentisdbd setup <agent|all> [--url <url>] [--dry-run]
  mentisdbd wizard [--url <url>] [--yes]

Supported agents:
  codex
  claude-code
  claude-desktop
  gemini
  opencode
  qwen
  copilot
  vscode-copilot

Commands:
  setup
    Write the canonical MentisDB MCP configuration for one supported agent,
    or for every supported integration with `all`.

    Examples:
      mentisdbd setup codex
      mentisdbd setup claude-desktop
      mentisdbd setup all --dry-run
      mentisdbd setup qwen --url http://127.0.0.1:9471

    Options:
      --url <url>   Override the default MentisDB MCP endpoint for the selected target(s)
      --dry-run     Print the setup plan without modifying files
      --help        Show this help text

  wizard
    Scan the local machine for supported clients, show detection status,
    let you choose integrations to configure, and apply changes interactively.

    Behavior:
      - Detects whether a mentisdb integration already exists per client
      - Lets you skip or overwrite existing mentisdb entries
      - `--yes` accepts default selections but still skips existing mentisdb entries
      - Uses per-integration default URLs unless you override them
      - For Claude Desktop, checks for npm and installs mcp-remote if needed

    Examples:
      mentisdbd wizard
      mentisdbd wizard --yes
      mentisdbd wizard --url https://my.mentisdb.com:9473

    Options:
      --url <url>   Override the default URL for all selected integrations
      --yes         Accept the wizard defaults without confirmation prompts
      --help        Show this help text

Notes:
  - `mentisdbd` with no subcommand starts the daemon.
  - `mentisdbd --help` shows daemon help; `mentisdbd setup --help` and
    `mentisdbd wizard --help` show the setup/wizard surface.
  - `setup` writes config files; it is not scaffold-only.
"
}

fn parse_setup(args: Vec<String>) -> Result<CliCommand, String> {
    if args.len() < 2 {
        return Err("setup requires <agent>".to_string());
    }
    if matches!(args[1].as_str(), "-h" | "--help" | "help") {
        return Ok(CliCommand::Help);
    }

    let integrations = if args[1] == "all" {
        IntegrationKind::ALL.to_vec()
    } else {
        vec![parse_integration(&args[1])
            .ok_or_else(|| format!("Unsupported agent '{}'", args[1]))?]
    };
    let mut url = None;
    let mut dry_run = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--url" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--url requires a value".to_string())?;
                url = Some(value.clone());
                index += 2;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for setup")),
        }
    }

    Ok(CliCommand::Setup(SetupCommand {
        url,
        integrations,
        dry_run,
    }))
}

fn parse_wizard(args: Vec<String>) -> Result<CliCommand, String> {
    let mut url = None;
    let mut assume_yes = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--url" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--url requires a value".to_string())?;
                url = Some(value.clone());
                index += 2;
            }
            "--yes" | "-y" => {
                assume_yes = true;
                index += 1;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for wizard")),
        }
    }

    Ok(CliCommand::Wizard(WizardCommand { url, assume_yes }))
}

pub(super) fn parse_integration(value: &str) -> Option<IntegrationKind> {
    match value.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(IntegrationKind::Codex),
        "claude" | "claude-code" | "claude_code" => Some(IntegrationKind::ClaudeCode),
        "claude-desktop" | "claude_desktop" | "desktop" => Some(IntegrationKind::ClaudeDesktop),
        "gemini" | "gemini-cli" | "gemini_cli" => Some(IntegrationKind::GeminiCli),
        "opencode" | "open-code" | "open_code" => Some(IntegrationKind::OpenCode),
        "qwen" | "qwen-code" | "qwen_code" => Some(IntegrationKind::Qwen),
        "copilot" | "copilot-cli" | "github-copilot" => Some(IntegrationKind::CopilotCli),
        "vscode-copilot" | "vscode_copilot" | "vscode" => Some(IntegrationKind::VsCodeCopilot),
        _ => None,
    }
}

pub(super) fn default_url(integration: IntegrationKind) -> &'static str {
    default_url_for_integration(integration)
}
