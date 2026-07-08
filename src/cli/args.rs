use crate::auth::BearerTokenScope;
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
    /// Optional bearer token for remote integrations (Authorization header).
    pub bearer_token: Option<String>,
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

/// Parsed `bearertoken` subcommand arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BearerTokenCommand {
    /// Create a new bearer token with the given alias.
    Create {
        /// Human-friendly token alias.
        alias: String,
        /// Token authorization scope.
        scope: BearerTokenScope,
        /// MentisDB storage directory.
        dir: Option<String>,
    },
    /// List bearer tokens.
    List {
        /// Optional token authorization scope filter.
        scope_filter: Option<BearerTokenScope>,
        /// MentisDB storage directory.
        dir: Option<String>,
    },
    /// Revoke a bearer token by alias.
    Remove {
        /// Human-friendly token alias.
        alias: String,
        /// MentisDB storage directory.
        dir: Option<String>,
    },
}

/// Parsed `cert` subcommand arguments.
///
/// Mints a self-signed certificate (and private key) and makes it the
/// active TLS material for the HTTPS MCP and REST servers by writing
/// `cert.pem` and `key.pem` to the location held in `MENTISDB_TLS_CERT`
/// (or the default `<MENTISDB_DIR>/tls/` path) and persisting those
/// paths into the `.env` file so the next daemon start picks them up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertCommand {
    /// Optional IP address or DNS hostname to add as a Subject Alternative
    /// Name on top of the standard set (loopback, interface IPs, the
    /// well-known `my.mentisdb.com`, and `MENTISDB_BIND_HOST`).
    ///
    /// IPv4 and IPv6 literals are added as IP SANs; everything else is
    /// added as a DNS SAN. The standard set is always included.
    pub host: Option<String>,
    /// When `true`, replace an existing cert and key on disk. When `false`
    /// (the default), the command refuses to overwrite an existing cert so
    /// operators do not accidentally invalidate an already-trusted cert.
    pub overwrite: bool,
    /// When `true`, delete any existing cert and key files before
    /// generating a new one. This ensures a clean factory-default
    /// certificate is produced even if the files were corrupted or
    /// manually edited. Implies `--force`.
    pub reset: bool,
    /// Override the output directory for `cert.pem` and `key.pem`. When
    /// `None`, the directory is resolved from `MENTISDB_TLS_CERT` (or its
    /// default).
    pub output_dir: Option<String>,
    /// Path to the `.env` file to update. When `None`, the dispatcher
    /// defaults to `.env` in the current working directory; if no such
    /// file exists the dispatcher simply prints the values to copy.
    pub env_file: Option<String>,
    /// Do not write anything to the `.env` file. The cert is still
    /// generated and printed, but operators are responsible for updating
    /// the environment themselves.
    pub no_env_update: bool,
}

/// Supported top-level commands for `mentisdb` CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    /// Print CLI help (comprehensive: daemon + all subcommands).
    Help,
    /// Print setup subcommand help.
    SetupHelp,
    /// Print wizard subcommand help.
    WizardHelp,
    /// Print add subcommand help.
    AddHelp,
    /// Print search subcommand help.
    SearchHelp,
    /// Print agents subcommand help.
    AgentsHelp,
    /// Print backup subcommand help.
    BackupHelp,
    /// Print restore subcommand help.
    RestoreHelp,
    /// Print bearer-token CLI help.
    BearerTokenHelp,
    /// Print cert subcommand help.
    CertHelp,
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
    /// Manage bearer tokens for MCP access.
    BearerToken(BearerTokenCommand),
    /// Mint a fresh self-signed TLS certificate for the HTTPS servers.
    Cert(CertCommand),
}

/// Parse command-line arguments for the embedded `mentisdb` setup and wizard CLI.
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

    // Check for subcommand-specific help flags
    let has_help_flag = parts
        .iter()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "help"));

    match subcommand.as_str() {
        "-h" | "--help" | "help" => Ok(CliCommand::Help),
        "setup" if has_help_flag => Ok(CliCommand::SetupHelp),
        "wizard" if has_help_flag => Ok(CliCommand::WizardHelp),
        "add" if has_help_flag => Ok(CliCommand::AddHelp),
        "search" if has_help_flag => Ok(CliCommand::SearchHelp),
        "agents" if has_help_flag => Ok(CliCommand::AgentsHelp),
        "backup" if has_help_flag => Ok(CliCommand::BackupHelp),
        "restore" if has_help_flag => Ok(CliCommand::RestoreHelp),
        "bearertoken" if has_help_flag => Ok(CliCommand::BearerTokenHelp),
        "cert" if has_help_flag => Ok(CliCommand::CertHelp),
        "setup" => parse_setup(parts),
        "wizard" => parse_wizard(parts),
        "add" => parse_add(parts),
        "search" => parse_search(parts),
        "agents" => parse_agents(parts),
        "backup" => parse_backup(parts),
        "restore" => parse_restore(parts),
        "bearertoken" => parse_bearer_token(parts),
        "cert" => parse_cert(parts),
        other => Err(format!("Unknown subcommand '{other}'")),
    }
}

/// Return the full `mentisdb --help` text. Composed from a static
/// header / subcommand index and the per-subcommand help blocks
/// exposed by [`super::cert::help_text`], [`bearer_token_help_text`],
/// etc. The function returns an owned [`String`] because the cert
/// block is appended at runtime so the source of truth for its
/// wording stays in the cert module.
#[must_use]
pub fn help_text() -> String {
    let mut text = String::from(HELP_HEADER);
    text.push_str(super::cert::help_text());
    text.push_str(HELP_FOOTER);
    text
}

const HELP_HEADER: &str = "\
mentisdb CLI

Usage:
  mentisdb --help
  mentisdb
  mentisdb --mode stdio
  mentisdb --mode http
  mentisdb --mode both
  mentisdb setup <agent|all> [--url <url>] [--dry-run] [--yes]
  mentisdb wizard [--url <url>] [--yes]
  mentisdb add <content> [--type <type>] [--scope <scope>] [--tag <tag>] [--agent <id>] [--chain <key>] [--url <url>]
  mentisdb search <query> [--limit <n>] [--scope <scope>] [--chain <key>] [--url <url>]
  mentisdb agents [--chain <key>] [--url <url>]
  mentisdb backup [-o <path>] [--dir <path>] [--flush] [--include-tls]
  mentisdb restore <archive.mentis> [--dir <path>] [--overwrite] [--yes]
  mentisdb bearertoken create --global <alias> [--dir <path>]
  mentisdb bearertoken create --chain <chain_key> [--chain <chain_key> ...] <alias> [--dir <path>]
  mentisdb bearertoken list [--global | --chain <chain_key> [--chain <chain_key> ...]] [--dir <path>]
  mentisdb bearertoken remove <alias> [--dir <path>]
  mentisdb cert [<ip-address-or-domain>] [--force] [--out-dir <path>] [--env-file <path>] [--no-env-update]

Client transport (--mode):
  --mode stdio     MCP over stdin/stdout for subprocess clients (Claude Desktop,
                   etc.). Usually proxies to the HTTP daemon for one shared
                   chain cache; see Flags above for the full stdio/http/both
                   descriptions.
  --mode http      HTTP MCP + REST (+ optional HTTPS) and the operator TUI.
                   Same as plain mentisdb; not HTTP-only — the default already
                   starts these services. Use with --headless to drop the TUI.
  --mode both      Stdio MCP and the HTTP/TUI operator stack in one process.
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
      mentisdb setup codex
      mentisdb setup claude-desktop
      mentisdb setup all --dry-run
      mentisdb setup all --yes
      mentisdb setup qwen --url http://127.0.0.1:9471

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
      mentisdb wizard
      mentisdb wizard --yes
      mentisdb wizard --url https://my.mentisdb.com:9473

    Options:
      --url <url>   Override the default URL for all selected integrations
      --yes         Accept the wizard defaults without confirmation prompts
      --help        Show this help text

  add
    Add a thought to a running MentisDB daemon via REST.

    Examples:
      mentisdb add \\\"The sky is blue\\\"
      mentisdb add \\\"Session fact\\\" --scope session --tag important
      mentisdb add \\\"Insight\\\" --type insight --agent my-agent

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
      mentisdb search \\\"cache invalidation\\\"
      mentisdb search \\\"performance\\\" --limit 5 --scope session

    Options:
      --limit <n>      Maximum results (default: 10)
      --scope <scope>  Filter by memory scope: user, session, or agent
      --chain <key>    Chain key (uses daemon default if omitted)
      --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
      --help           Show this help text

  agents
    List registered agents on a running MentisDB daemon.

    Examples:
      mentisdb agents
      mentisdb agents --chain my-chain

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
      mentisdb backup
      mentisdb backup -o /backups/mentisdb-2026-04-14.mentis
      mentisdb backup --dir ~/.cloudllm/mentisdb --flush --include-tls

    Options:
      -o <path>          Path for the .mentis archive (default: ./mentisdb-YYYY-MM-DD-HH-MM-SS.mentis)
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --flush            Flush all storage adapters before backing up (recommended if daemon is running)
      --include-tls      Include TLS certificates and keys in the backup
      --help             Show this help text

  restore
    Restore a MENTISDB_DIR from a .mentis backup archive.

    Restores all chain data, registry, skills, and optionally TLS files.
    Empty targets receive a full instance restore. Non-empty targets preserve
    the local registry, add newly discovered backup chains, append verified
    same-key suffixes, and import divergent same-name chains under a renamed key.

    The daemon must not be running during restore. If mentisdb is detected,
    the restore aborts with a message to stop the daemon first. This prevents
    the daemon's in-memory state from overwriting restored files.

    --overwrite is chain-scoped: it can replace same-path chain files when a
    safe suffix merge is not possible. It does not blindly replace the local
    registry, skills, webhooks, TLS, or unrelated chains.

    Examples:
      mentisdb restore mentisdb-2026-04-14.mentis
      mentisdb restore /backups/mentisdb-2026-04-14.mentis --dir ~/.cloudllm/mentisdb
      mentisdb restore /backups/mentisdb-2026-04-14.mentis --overwrite

    Options:
      <archive.mentis>   Path to the .mentis backup archive (required)
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --overwrite        Allow same-path chain files to be replaced when needed
      --yes              Skip interactive confirmation prompts
      --help             Show this help text

  bearertoken
    Manage bearer tokens used when MENTISDB_BEARER_TOKEN_ACCESS=true.

    Run `mentisdb bearertoken --help` for focused bearer-token help.

    The raw token is printed only once by `create`; MentisDB stores only a
    token hash in the bearer-token registry.

    Examples:
      mentisdb bearertoken create --global codex-admin
      mentisdb bearertoken create --chain mentisdb --chain gubatron codex-laptop
      mentisdb bearertoken list
      mentisdb bearertoken remove codex-laptop

    Options:
      --dir <path>       Path to MENTISDB_DIR (default: platform default)
      --help             Show this help text

";

const HELP_FOOTER: &str = "\
Notes:
  - `mentisdb` with no subcommand starts the daemon.
  - `mentisdb --help` shows daemon help; subcommand --help shows subcommand help.
  - `setup` writes config files; it is not scaffold-only.
  - `add`, `search`, and `agents` require a running daemon.
  - `backup` and `restore` operate on the MENTISDB_DIR directly and do not require a running daemon.
";

/// Return the help text for the `bearertoken` subcommand.
pub fn bearer_token_help_text() -> &'static str {
    "\
Manage bearer tokens for MCP access.

Bearer tokens are enforced only when MENTISDB_BEARER_TOKEN_ACCESS=true.
Existing tokens can be created, listed, and revoked regardless of the setting.

Usage:
  mentisdb bearertoken create --global <alias>
  mentisdb bearertoken create --chain <chain_key> [--chain <chain_key> ...] <alias>
  mentisdb bearertoken create <alias> --chain <chain_key> [--chain <chain_key> ...]
  mentisdb bearertoken list [--global | --chain <chain_key> [--chain <chain_key> ...]] [--dir <path>]
  mentisdb bearertoken remove <alias> [--dir <path>]

Create scopes:
  --global             Token can access all chains.
  --chain <chain_key>  Token can access this chain. Repeat for multiple chains.

Examples:
  mentisdb bearertoken create --global codex-admin
  mentisdb bearertoken create --chain gubatron alice
  mentisdb bearertoken create alice --chain gubatron --chain mentisdb
  mentisdb bearertoken list
  mentisdb bearertoken list --chain gubatron
  mentisdb bearertoken remove alice

Options:
  --dir <path>         Path to MENTISDB_DIR (default: platform default)
  --help               Show this help text
"
}

/// Return the help text for the `setup` subcommand.
pub fn setup_help_text() -> &'static str {
    "\
mentisdb setup — Write the canonical MentisDB MCP configuration for one supported agent, or for every supported integration with `all`.

Usage:
  mentisdb setup <agent|all> [--url <url>] [--dry-run] [--yes]

Supported agents:
  codex, claude-code, claude-desktop, gemini, opencode, qwen, copilot, vscode-copilot

Options:
  --url <url>     Override the default MentisDB MCP endpoint for the selected target(s)
  --dry-run       Print the setup plan without modifying files
  --yes           Apply the rendered plan without the final confirmation prompt
  --help          Show this help text

Examples:
  mentisdb setup codex
  mentisdb setup claude-desktop
  mentisdb setup all --dry-run
  mentisdb setup all --yes
  mentisdb setup qwen --url http://127.0.0.1:9471
"
}

/// Return the help text for the `wizard` subcommand.
pub fn wizard_help_text() -> &'static str {
    "\
mentisdb wizard — Scan the local machine for supported clients, show detection status, let you choose integrations to configure, and apply changes interactively.

Usage:
  mentisdb wizard [--url <url>] [--yes]

Behavior:
  - Detects whether a mentisdb integration already exists per client
  - Lets you skip or overwrite existing mentisdb entries
  - `--yes` accepts default selections but still skips existing mentisdb entries
  - Uses per-integration default URLs unless you override them
  - For Claude Desktop, checks for npm and installs mcp-remote if needed

Options:
  --url <url>   Override the default URL for all selected integrations
  --yes         Accept the wizard defaults without confirmation prompts
  --help        Show this help text

Examples:
  mentisdb wizard
  mentisdb wizard --yes
  mentisdb wizard --url https://my.mentisdb.com:9473
"
}

/// Return the help text for the `add` subcommand.
pub fn add_help_text() -> &'static str {
    "\
mentisdb add — Add a thought to a running MentisDB daemon via REST.

Usage:
  mentisdb add <content> [--type <type>] [--scope <scope>] [--tag <tag>] [--agent <id>] [--chain <key>] [--url <url>]

Options:
  --type <type>    Thought type (default: fact-learned)
  --scope <scope>  Memory scope: user, session, or agent
  --tag <tag>      Add a tag (repeatable)
  --agent <id>     Agent ID for the thought
  --chain <key>    Chain key (uses daemon default if omitted)
  --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
  --help           Show this help text

Examples:
  mentisdb add \"The sky is blue\"
  mentisdb add \"Session fact\" --scope session --tag important
  mentisdb add \"Insight\" --type insight --agent my-agent
"
}

/// Return the help text for the `search` subcommand.
pub fn search_help_text() -> &'static str {
    "\
mentisdb search — Search thoughts on a running MentisDB daemon via ranked search.

Usage:
  mentisdb search <query> [--limit <n>] [--scope <scope>] [--chain <key>] [--url <url>]

Options:
  --limit <n>      Maximum results (default: 10)
  --scope <scope>  Filter by memory scope: user, session, or agent
  --chain <key>    Chain key (uses daemon default if omitted)
  --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
  --help           Show this help text

Examples:
  mentisdb search \"cache invalidation\"
  mentisdb search \"performance\" --limit 5 --scope session
"
}

/// Return the help text for the `agents` subcommand.
pub fn agents_help_text() -> &'static str {
    "\
mentisdb agents — List registered agents on a running MentisDB daemon.

Usage:
  mentisdb agents [--chain <key>] [--url <url>]

Options:
  --chain <key>    Chain key (uses daemon default if omitted)
  --url <url>      Daemon REST URL (default: http://127.0.0.1:9472)
  --help           Show this help text

Examples:
  mentisdb agents
  mentisdb agents --chain my-chain
"
}

/// Return the help text for the `backup` subcommand.
pub fn backup_help_text() -> &'static str {
    "\
mentisdb backup — Create a .mentis backup archive of the MENTISDB_DIR.

Usage:
  mentisdb backup [-o <path>] [--dir <path>] [--flush] [--include-tls]

The backup includes all chain data files (*.tcbin, *.agents.json,
*.entity-types.json, *.vectors.*.json), the global registry, and
optionally TLS certificates and keys.

If the daemon is running, all chains are flushed before reading files
for a consistent backup. If the daemon is not running, files are captured
as-is.

Options:
  -o <path>          Path for the .mentis archive (default: ./mentisdb-YYYY-MM-DD-HH-MM-SS.mentis)
  --dir <path>       Path to MENTISDB_DIR (default: platform default)
  --flush            Flush all storage adapters before backing up (recommended if daemon is running)
  --include-tls      Include TLS certificates and keys in the backup
  --help             Show this help text

Examples:
  mentisdb backup
  mentisdb backup -o /backups/mentisdb-2026-04-14.mentis
  mentisdb backup --dir ~/.cloudllm/mentisdb --flush --include-tls
"
}

/// Return the help text for the `restore` subcommand.
pub fn restore_help_text() -> &'static str {
    "\
mentisdb restore — Restore a MENTISDB_DIR from a .mentis backup archive.

Usage:
  mentisdb restore <archive.mentis> [--dir <path>] [--overwrite] [--yes]

Restores all chain data, registry, skills, and optionally TLS files.
Empty targets receive a full instance restore. Non-empty targets preserve
the local registry, add newly discovered backup chains, append verified
same-key suffixes, and import divergent same-name chains under a renamed key.

The daemon must not be running during restore. If mentisdb is detected,
the restore aborts with a message to stop the daemon first. This prevents
the daemon's in-memory state from overwriting restored files.

--overwrite is chain-scoped: it can replace same-path chain files when a
safe suffix merge is not possible. It does not blindly replace the local
registry, skills, webhooks, TLS, or unrelated chains.

Options:
  <archive.mentis>   Path to the .mentis backup archive (required)
  --dir <path>       Path to MENTISDB_DIR (default: platform default)
  --overwrite        Allow same-path chain files to be replaced when needed
  --yes              Skip interactive confirmation prompts
  --help             Show this help text

Examples:
  mentisdb restore mentisdb-2026-04-14.mentis
  mentisdb restore /backups/mentisdb-2026-04-14.mentis --dir ~/.cloudllm/mentisdb
  mentisdb restore /backups/mentisdb-2026-04-14.mentis --overwrite
"
}

/// Return the help text for the `cert` subcommand.
pub fn cert_help_text() -> &'static str {
    super::cert::help_text()
}

fn parse_bearer_token(parts: Vec<String>) -> Result<CliCommand, String> {
    if parts.len() == 1
        || parts
            .iter()
            .skip(1)
            .any(|part| matches!(part.as_str(), "--help" | "-h" | "help"))
    {
        return Ok(CliCommand::BearerTokenHelp);
    }

    let Some(action) = parts.get(1).map(String::as_str) else {
        return Err("bearertoken requires an action: create, list, or remove".to_string());
    };
    let mut dir = None;
    let mut global_scope = false;
    let mut chain_keys = Vec::new();
    let mut positional = Vec::new();
    let mut i = 2;
    while i < parts.len() {
        match parts[i].as_str() {
            "--global" => {
                if global_scope || !chain_keys.is_empty() {
                    return Err(
                        "bearertoken accepts either --global or one or more --chain <chain_key> options"
                            .to_string(),
                    );
                }
                global_scope = true;
            }
            "--chain" => {
                if global_scope {
                    return Err(
                        "bearertoken accepts either --global or one or more --chain <chain_key> options"
                            .to_string(),
                    );
                }
                i += 1;
                let chain_key = parts
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--chain requires a value".to_string())?;
                chain_keys.push(chain_key);
            }
            "--dir" => {
                i += 1;
                dir = Some(
                    parts
                        .get(i)
                        .cloned()
                        .ok_or_else(|| "--dir requires a value".to_string())?,
                );
            }
            other if other.starts_with('-') => {
                return Err(format!("Unknown bearertoken option '{other}'"));
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    let command = match action {
        "create" => {
            let alias = exactly_one_arg("bearertoken create", positional)?;
            let scope = bearer_token_scope_from_flags(global_scope, chain_keys)?;
            BearerTokenCommand::Create { alias, scope, dir }
        }
        "list" => {
            if !positional.is_empty() {
                return Err("bearertoken list does not accept positional arguments".to_string());
            }
            let scope = optional_bearer_token_scope_from_flags(global_scope, chain_keys)?;
            BearerTokenCommand::List {
                scope_filter: scope,
                dir,
            }
        }
        "remove" | "rm" | "revoke" => {
            if global_scope || !chain_keys.is_empty() {
                return Err("bearertoken remove does not accept scope options".to_string());
            }
            let alias = exactly_one_arg("bearertoken remove", positional)?;
            BearerTokenCommand::Remove { alias, dir }
        }
        other => return Err(format!("Unknown bearertoken action '{other}'")),
    };
    Ok(CliCommand::BearerToken(command))
}

fn bearer_token_scope_from_flags(
    global_scope: bool,
    chain_keys: Vec<String>,
) -> Result<BearerTokenScope, String> {
    if global_scope {
        return Ok(BearerTokenScope::Global);
    }
    if chain_keys.is_empty() {
        return Err(
            "bearertoken create requires exactly one scope: --global or at least one --chain <chain_key>"
                .to_string(),
        );
    }
    BearerTokenScope::chains(chain_keys).map_err(|error| error.to_string())
}

fn optional_bearer_token_scope_from_flags(
    global_scope: bool,
    chain_keys: Vec<String>,
) -> Result<Option<BearerTokenScope>, String> {
    if global_scope {
        return Ok(Some(BearerTokenScope::Global));
    }
    if chain_keys.is_empty() {
        return Ok(None);
    }
    BearerTokenScope::chains(chain_keys)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn exactly_one_arg(command: &str, positional: Vec<String>) -> Result<String, String> {
    match positional.as_slice() {
        [arg] => Ok(arg.clone()),
        [] => Err(format!("{command} requires an alias")),
        _ => Err(format!("{command} accepts exactly one alias")),
    }
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
    let mut bearer_token = None;
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
            "--bearer" | "--token" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--bearer requires a value".to_string())?;
                bearer_token = Some(value.clone());
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

    Ok(CliCommand::Wizard(WizardCommand {
        url,
        bearer_token,
        assume_yes,
    }))
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

fn parse_cert(args: Vec<String>) -> Result<CliCommand, String> {
    if matches!(
        args.get(1).map(|s| s.as_str()),
        Some("-h" | "--help" | "help")
    ) {
        return Ok(CliCommand::Help);
    }

    let mut host: Option<String> = None;
    let mut overwrite = false;
    let mut reset = false;
    let mut output_dir: Option<String> = None;
    let mut env_file: Option<String> = None;
    let mut no_env_update = false;
    let mut index = 1;

    while index < args.len() {
        let arg = args[index].as_str();
        if arg.starts_with('-') {
            match arg {
                "--force" | "--overwrite" => {
                    overwrite = true;
                    index += 1;
                }
                "--reset" => {
                    reset = true;
                    overwrite = true; // --reset implies --force
                    index += 1;
                }
                "--out-dir" | "--out" => {
                    let value = args
                        .get(index + 1)
                        .cloned()
                        .ok_or_else(|| format!("{arg} requires a value"))?;
                    if value.trim().is_empty() {
                        return Err(format!("{arg} requires a non-empty value"));
                    }
                    output_dir = Some(value);
                    index += 2;
                }
                "--env-file" => {
                    let value = args
                        .get(index + 1)
                        .cloned()
                        .ok_or_else(|| "--env-file requires a value".to_string())?;
                    if value.trim().is_empty() {
                        return Err("--env-file requires a non-empty value".to_string());
                    }
                    env_file = Some(value);
                    index += 2;
                }
                "--no-env-update" => {
                    no_env_update = true;
                    index += 1;
                }
                "-h" | "--help" => return Ok(CliCommand::Help),
                other => return Err(format!("Unexpected argument '{other}' for cert")),
            }
        } else if host.is_none() {
            host = Some(arg.to_string());
            index += 1;
        } else {
            return Err(format!(
                "cert accepts at most one positional <ip-or-domain> argument; got '{arg}'"
            ));
        }
    }

    if let Some(value) = host.as_ref() {
        if value.trim().is_empty() {
            return Err("cert <ip-or-domain> cannot be empty".to_string());
        }
    }

    Ok(CliCommand::Cert(CertCommand {
        host,
        overwrite,
        reset,
        output_dir,
        env_file,
        no_env_update,
    }))
}
