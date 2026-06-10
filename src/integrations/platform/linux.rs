use super::PlatformPaths;
use crate::integrations::{
    IntegrationFileFormat, IntegrationKind, IntegrationPathTarget, IntegrationSpec,
};
use crate::paths::{HostPlatform, PathEnvironment};

pub(super) fn specs(platform: HostPlatform, env: &PathEnvironment) -> Vec<IntegrationSpec> {
    let paths = PlatformPaths::new(env, platform);

    vec![
        IntegrationSpec {
            integration: IntegrationKind::Codex,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.home.join(".codex").join("config.toml"),
                "Primary Codex MCP and CLI config",
                IntegrationFileFormat::Toml,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.home.join(".codex"),
                "Codex home directory",
            )],
            companion_targets: vec![],
            notes: vec!["Linux follows the same ~/.codex layout as macOS.".into()],
        },
        IntegrationSpec {
            integration: IntegrationKind::ClaudeCode,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.home.join(".claude.json"),
                "Claude Code global config and MCP servers",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.home.join(".claude"),
                "Claude Code home directory",
            )],
            companion_targets: vec![IntegrationPathTarget::file(
                paths.home.join(".claude").join("mcp").join("mentisdb.json"),
                "Legacy Claude Code per-server MCP file",
                IntegrationFileFormat::Json,
            )],
            notes: vec![
                "Linux Claude Code MCP servers are configured under ~/.claude.json (mcpServers.mentisdb); ~/.claude/mcp/mentisdb.json is treated as legacy.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::GeminiCli,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.home.join(".gemini").join("settings.json"),
                "Primary Gemini CLI settings",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.home.join(".gemini"),
                "Gemini CLI home directory",
            )],
            companion_targets: vec![IntegrationPathTarget::file(
                paths.home.join(".gemini").join("system.md"),
                "Optional Gemini system prompt file",
                IntegrationFileFormat::Markdown,
            )],
            notes: vec![
                "Gemini Linux support is path-mapped from the macOS layout; host validation should happen when the writer lands.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::OpenCode,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.config_root.join("opencode").join("opencode.json"),
                "Primary OpenCode settings",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.config_root.join("opencode"),
                "OpenCode config directory",
            )],
            companion_targets: vec![],
            notes: vec!["OpenCode uses an XDG-style config directory on Linux.".into()],
        },
        IntegrationSpec {
            integration: IntegrationKind::Qwen,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.home.join(".qwen").join("settings.json"),
                "Primary Qwen settings",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.home.join(".qwen"),
                "Qwen home directory",
            )],
            companion_targets: vec![],
            notes: vec!["Qwen Linux support follows ~/.qwen/settings.json.".into()],
        },
        IntegrationSpec {
            integration: IntegrationKind::CopilotCli,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.copilot_root.join("mcp-config.json"),
                "GitHub Copilot CLI MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.copilot_root,
                "GitHub Copilot CLI home directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "GitHub Copilot CLI uses ~/.copilot/mcp-config.json by default and XDG_CONFIG_HOME/copilot/mcp-config.json when XDG_CONFIG_HOME is set.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::VsCodeCopilot,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.config_root.join("Code").join("User").join("mcp.json"),
                "VS Code MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.config_root.join("Code").join("User"),
                "VS Code user settings directory",
            )],
            companion_targets: vec![IntegrationPathTarget::file(
                paths.config_root.join("Code").join("User").join("settings.json"),
                "Existing VS Code user settings",
                IntegrationFileFormat::Json,
            )],
            notes: vec![
                "Use the Code/User directory as the installation signal even if mcp.json has not been created yet.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::ClaudeDesktop,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.config_root
                    .join("Claude")
                    .join("claude_desktop_config.json"),
                "Claude Desktop MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.config_root.join("Claude"),
                "Claude Desktop config directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "Linux Claude Desktop support is path-mapped to ~/.config/Claude/claude_desktop_config.json.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::Grok,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.config_root.join("grok").join("config.toml"),
                "Grok CLI global config (MCP servers)",
                IntegrationFileFormat::Toml,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.config_root.join("grok"),
                "Grok CLI config directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "Grok CLI stores MCP servers under [mcp_servers.mentisdb] in ~/.config/grok/config.toml (or ~/.grok).".into(),
            ],
        },
    ]
}
