use super::PlatformPaths;
use crate::integrations::{
    IntegrationFileFormat, IntegrationKind, IntegrationPathTarget, IntegrationSpec,
};
use crate::paths::{HostPlatform, PathEnvironment};

pub(super) fn specs(env: &PathEnvironment) -> Vec<IntegrationSpec> {
    let platform = HostPlatform::Windows;
    let paths = PlatformPaths::new(env, platform);
    let app_data = env
        .config_root_for(platform)
        .unwrap_or_else(|| paths.home.join("AppData").join("Roaming"));

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
            notes: vec!["Windows Codex support maps to %USERPROFILE%\\.codex\\config.toml.".into()],
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
                "Windows Claude Code MCP servers are configured under %USERPROFILE%\\.claude.json (mcpServers.mentisdb); %USERPROFILE%\\.claude\\mcp\\mentisdb.json is treated as legacy.".into(),
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
            notes: vec!["Windows Gemini path mapping mirrors the user-home layout used on macOS.".into()],
        },
        IntegrationSpec {
            integration: IntegrationKind::OpenCode,
            platform,
            config_target: IntegrationPathTarget::file(
                app_data.join("opencode").join("opencode.json"),
                "Primary OpenCode settings",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                app_data.join("opencode"),
                "OpenCode config directory",
            )],
            companion_targets: vec![],
            notes: vec!["Windows OpenCode support maps to %APPDATA%\\opencode\\opencode.json.".into()],
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
            notes: vec!["Windows Qwen support maps to %USERPROFILE%\\.qwen\\settings.json.".into()],
        },
        IntegrationSpec {
            integration: IntegrationKind::CopilotCli,
            platform,
            config_target: IntegrationPathTarget::file(
                paths.home.join(".copilot").join("mcp-config.json"),
                "GitHub Copilot CLI MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                paths.home.join(".copilot"),
                "GitHub Copilot CLI home directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "Windows GitHub Copilot CLI support maps to %USERPROFILE%\\.copilot\\mcp-config.json.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::VsCodeCopilot,
            platform,
            config_target: IntegrationPathTarget::file(
                app_data.join("Code").join("User").join("mcp.json"),
                "VS Code MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                app_data.join("Code").join("User"),
                "VS Code user settings directory",
            )],
            companion_targets: vec![IntegrationPathTarget::file(
                app_data.join("Code").join("User").join("settings.json"),
                "Existing VS Code user settings",
                IntegrationFileFormat::Json,
            )],
            notes: vec![
                "Treat %APPDATA%\\Code\\User as the installation signal even when mcp.json is absent.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::ClaudeDesktop,
            platform,
            config_target: IntegrationPathTarget::file(
                app_data.join("Claude").join("claude_desktop_config.json"),
                "Claude Desktop MCP configuration",
                IntegrationFileFormat::Json,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                app_data.join("Claude"),
                "Claude Desktop config directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "Windows Claude Desktop support maps to %APPDATA%\\Claude\\claude_desktop_config.json.".into(),
            ],
        },
        IntegrationSpec {
            integration: IntegrationKind::Grok,
            platform,
            config_target: IntegrationPathTarget::file(
                app_data.join("grok").join("config.toml"),
                "Grok CLI global config (MCP servers)",
                IntegrationFileFormat::Toml,
            ),
            detection_probes: vec![IntegrationPathTarget::directory(
                app_data.join("grok"),
                "Grok CLI config directory",
            )],
            companion_targets: vec![],
            notes: vec![
                "Grok CLI stores MCP servers under [mcp_servers.mentisdb] in %APPDATA%\\grok\\config.toml.".into(),
            ],
        },
    ]
}
