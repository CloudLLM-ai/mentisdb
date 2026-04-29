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
    /// Apply the rendered setup plan without prompting for confirmation.
    pub assume_yes: bool,
}

/// Parsed `wizard` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardCommand {
    /// Optional preselected MentisDB MCP endpoint URL.
    pub url: Option<String>,
    /// Apply the default detected selection without prompting for confirmation.
    pub assume_yes: bool,
}

/// Parsed `add` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddCommand {
    /// The content to add as a thought.
    pub content: String,
    /// Optional thought type (defaults to "fact-learned").
    pub thought_type: Option<String>,
    /// Optional memory scope.
    pub scope: Option<String>,
    /// Optional tags.
    pub tags: Vec<String>,
    /// Optional agent ID.
    pub agent_id: Option<String>,
    /// Optional chain key.
    pub chain_key: Option<String>,
    /// Daemon REST URL.
    pub url: String,
}

/// Parsed `search` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCommand {
    /// The search query text.
    pub text: String,
    /// Maximum number of results.
    pub limit: Option<usize>,
    /// Optional memory scope filter.
    pub scope: Option<String>,
    /// Optional chain key.
    pub chain_key: Option<String>,
    /// Daemon REST URL.
    pub url: String,
}

/// Parsed `agents` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsCommand {
    /// Optional chain key.
    pub chain_key: Option<String>,
    /// Daemon REST URL.
    pub url: String,
}

/// Parsed `backup` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCommand {
    /// Path to the source MENTISDB_DIR (defaults to the platform default).
    pub source_dir: Option<String>,
    /// Path where the .mentis archive should be written.
    pub output_path: Option<String>,
    /// Flush all storage adapters before backing up.
    pub flush: bool,
    /// Include TLS certificates and keys in the backup.
    pub include_tls: bool,
}

/// Parsed `restore` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCommand {
    /// Path to the .mentis archive to restore.
    pub archive_path: String,
    /// Path to the target MENTISDB_DIR (defaults to the platform default).
    pub target_dir: Option<String>,
    /// Overwrite existing files in the target directory.
    pub overwrite: bool,
    /// Skip interactive prompts and assume yes.
    pub yes: bool,
}

/// Supported top-level commands for `mentisdbd` CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Print CLI help.
    Help,
    /// Print a setup scaffold for one target agent.
    Setup(SetupCommand),
    /// Run the interactive setup wizard.
    Wizard(WizardCommand),
    /// Add a thought to a running daemon.
    Add(AddCommand),
    /// Search thoughts on a running daemon.
    Search(SearchCommand),
    /// List agents on a running daemon.
    Agents(AgentsCommand),
    /// Create a backup archive.
    Backup(BackupCommand),
    /// Restore from a backup archive.
    Restore(RestoreCommand),
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
        "add" => parse_add(parts),
        "search" => parse_search(parts),
        "agents" => parse_agents(parts),
        "backup" => parse_backup(parts),
        "restore" => parse_restore(parts),
        other => Err(format!("Unknown subcommand '{other}'")),
    }
}

pub(crate) fn help_text() -> &'static str {
    "\
mentisdbd CLI

Usage:
  mentisdbd --help
  mentisdbd
  mentisdbd --mode stdio
  mentisdbd --mode http
  mentisdbd --mode both
  mentisdbd setup <agent|all> [--url <url>] [--dry-run] [--yes]
  mentisdbd wizard [--url <url>] [--yes]
  mentisdbd add <content> [--type <type>] [--scope <scope>] [--tag <tag>] [--agent <id>] [--chain <key>] [--url <url>]
  mentisdbd search <query> [--limit <n>] [--scope <scope>] [--chain <key>] [--url <url>]
  mentisdbd agents [--chain <key>] [--url <url>]
  mentisdbd backup [-o <path>] [--dir <path>] [--flush] [--include-tls]
  mentisdbd restore <archive.mentis> [--dir <path>] [--overwrite] [--yes]

Daemon modes (start HTTP servers by default):
  --mode stdio     Start MCP server over stdio (for Claude Desktop subprocess)
  --mode http      HTTP servers only (same as default)
  --mode both      Run both stdio MCP and HTTP servers
  --stdio-mcp      Alias for --mode stdio

Supported agents (setup/wizard):
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
      mentisdbd setup all --yes
      mentisdbd setup qwen --url http://127.0.0.1:9471

    Options:
      --url <url>   Override the default MentisDB MCP endpoint for the selected target(s)
      --dry-run     Print the setup plan without modifying files
      --yes         Apply the rendered plan without the final confirmation prompt
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

  add
    Add a thought to a running MentisDB daemon via REST.

    Examples:
      mentisdbd add \"The sky is blue\"
      mentisdbd add \"Session fact\" --scope session --tag important
      mentisdbd add \"Insight\" --type insight --agent my-agent

    Options:
      --type <type>    Thought type (default: fact-learned)
      --scope <scope>  Memory scope: user, session, or agent
      --tag <tag>      Add a tag (repeatable)
      --agent <id>     Agent ID for the thought
      --chain <key>    Chain key (uses daemon default if omitted)
      --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
      --help           Show this help text

  search
    Search thoughts on a running MentisDB daemon via ranked search.

    Examples:
      mentisdbd search \"cache invalidation\"
      mentisdbd search \"performance\" --limit 5 --scope session

    Options:
      --limit <n>      Maximum results (default: 10)
      --scope <scope>  Filter by memory scope: user, session, or agent
      --chain <key>    Chain key (uses daemon default if omitted)
      --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
      --help           Show this help text

  agents
    List registered agents on a running MentisDB daemon.

    Examples:
      mentisdbd agents
      mentisdbd agents --chain my-chain

    Options:
      --chain <key>    Chain key (uses daemon default if omitted)
      --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
      --help           Show this help text

  backup
    Create a .mentis backup archive of the MENTISDB_DIR.

    The backup includes all chain data files (*.tcbin, *.agents.json,
    *.entity-types.json, *.vectors.*.json), the global registry, and
    optionally TLS certificates and keys.

    If the daemon is running, all chains are flushed before reading files
    for a consistent backup. If the daemon is not running, files are captured
    as-is.

    Examples:
      mentisdbd backup
      mentisdbd backup -o /backups/mentisdb-2026-04-14.mentis
      mentisdbd backup --dir ~/.cloudllm/mentisdb --flush --include-tls

    Options:
      -o <path>          Path for the .mentis archive (default: ./mentisdb-YYYY-MM-DD-HH-MM-SS.mentis)
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --flush            Flush all storage adapters before backing up (recommended if daemon is running)
      --include-tls      Include TLS certificates and keys in the backup
      --help             Show this help text

  restore
    Restore a MENTISDB_DIR from a .mentis backup archive.

    Restores all chain data, registry, skills, and optionally TLS files.
    By default, existing files are preserved (idempotent). Pass --overwrite
    to replace all files with their backed-up versions.

    The daemon must not be running during restore. If mentisdbd is detected,
    the restore aborts with a message to stop the daemon first. This prevents
    the daemon's in-memory state from overwriting restored files.

    If files already exist in the target directory and --overwrite is not
    passed, an interactive prompt asks for confirmation before overwriting.

    Examples:
      mentisdbd restore mentisdb-2026-04-14.mentis
      mentisdbd restore /backups/mentisdb-2026-04-14.mentis --dir ~/.cloudllm/mentisdb
      mentisdbd restore /backups/mentisdb-2026-04-14.mentis --overwrite

    Options:
      <archive.mentis>   Path to the .mentis backup archive (required)
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --overwrite        Overwrite existing files in the target directory (skips interactive prompt)
      --yes              Assume yes to all prompts (skips interactive confirmation)
      --help             Show this help text

Notes:
  - `mentisdbd` with no subcommand starts the daemon.
  - `mentisdbd --help` shows daemon help; subcommand --help shows subcommand help.
  - `setup` writes config files; it is not scaffold-only.
  - `add`, `search`, and `agents` require a running daemon.
  - `backup` and `restore` operate on the MENTISDB_DIR directly and do not require a running daemon.
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
    let mut assume_yes = false;
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
            "--yes" | "-y" => {
                assume_yes = true;
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
        assume_yes,
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

fn default_rest_url() -> String {
    "http://127.0.0.1:9472".to_string()
}

fn parse_add(args: Vec<String>) -> Result<CliCommand, String> {
    if args.len() < 2 {
        return Err("add requires <content>".to_string());
    }
    if matches!(args[1].as_str(), "-h" | "--help" | "help") {
        return Ok(CliCommand::Help);
    }
    let content = args[1].clone();
    let mut thought_type = None;
    let mut scope = None;
    let mut tags = Vec::new();
    let mut agent_id = None;
    let mut chain_key = None;
    let mut url = default_rest_url();
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--type" => {
                thought_type = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--type requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--scope" => {
                scope = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--scope requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--tag" => {
                tags.push(
                    args.get(index + 1)
                        .ok_or_else(|| "--tag requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--agent" => {
                agent_id = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--agent requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--chain" => {
                chain_key = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--chain requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--url" => {
                url = args
                    .get(index + 1)
                    .ok_or_else(|| "--url requires a value".to_string())?
                    .clone();
                index += 2;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for add")),
        }
    }
    Ok(CliCommand::Add(AddCommand {
        content,
        thought_type,
        scope,
        tags,
        agent_id,
        chain_key,
        url,
    }))
}

fn parse_search(args: Vec<String>) -> Result<CliCommand, String> {
    if args.len() < 2 {
        return Err("search requires <query>".to_string());
    }
    if matches!(args[1].as_str(), "-h" | "--help" | "help") {
        return Ok(CliCommand::Help);
    }
    let text = args[1].clone();
    let mut limit = None;
    let mut scope = None;
    let mut chain_key = None;
    let mut url = default_rest_url();
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--limit" => {
                limit = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--limit requires a value".to_string())?
                        .parse::<usize>()
                        .map_err(|_| "invalid --limit value")?,
                );
                index += 2;
            }
            "--scope" => {
                scope = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--scope requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--chain" => {
                chain_key = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--chain requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--url" => {
                url = args
                    .get(index + 1)
                    .ok_or_else(|| "--url requires a value".to_string())?
                    .clone();
                index += 2;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for search")),
        }
    }
    Ok(CliCommand::Search(SearchCommand {
        text,
        limit,
        scope,
        chain_key,
        url,
    }))
}

fn parse_agents(args: Vec<String>) -> Result<CliCommand, String> {
    let mut chain_key = None;
    let mut url = default_rest_url();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--chain" => {
                chain_key = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--chain requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--url" => {
                url = args
                    .get(index + 1)
                    .ok_or_else(|| "--url requires a value".to_string())?
                    .clone();
                index += 2;
            }
            "-h" | "--help" | "help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for agents")),
        }
    }
    Ok(CliCommand::Agents(AgentsCommand { chain_key, url }))
}

fn parse_backup(args: Vec<String>) -> Result<CliCommand, String> {
    if matches!(
        args.get(1).map(|s| s.as_str()),
        Some("-h" | "--help" | "help")
    ) {
        return Ok(CliCommand::Help);
    }
    let mut source_dir = None;
    let mut output_path = None;
    let mut flush = false;
    let mut include_tls = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--dir" => {
                source_dir = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--dir requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "-o" | "--output" => {
                output_path = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "-o requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--flush" => {
                flush = true;
                index += 1;
            }
            "--include-tls" => {
                include_tls = true;
                index += 1;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for backup")),
        }
    }
    Ok(CliCommand::Backup(BackupCommand {
        source_dir,
        output_path,
        flush,
        include_tls,
    }))
}

fn parse_restore(args: Vec<String>) -> Result<CliCommand, String> {
    if matches!(
        args.get(1).map(|s| s.as_str()),
        Some("-h" | "--help" | "help")
    ) {
        return Ok(CliCommand::Help);
    }
    let archive_path = args
        .get(1)
        .ok_or_else(|| "restore requires <archive.mentis>".to_string())?
        .clone();
    let mut target_dir = None;
    let mut overwrite = false;
    let mut yes = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--dir" => {
                target_dir = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--dir requires a value".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--overwrite" => {
                overwrite = true;
                index += 1;
            }
            "--yes" | "-y" => {
                yes = true;
                index += 1;
            }
            "-h" | "--help" => return Ok(CliCommand::Help),
            other => return Err(format!("Unexpected argument '{other}' for restore")),
        }
    }
    Ok(CliCommand::Restore(RestoreCommand {
        archive_path,
        target_dir,
        overwrite,
        yes,
    }))
}
