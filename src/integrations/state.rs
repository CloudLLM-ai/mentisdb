//! Shared state for concrete integration apply plans.

use crate::integrations::files::ManagedFile;
use crate::integrations::IntegrationKind;
use crate::paths::HostPlatform;

/// Shared writer settings for MentisDB host integrations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrationWriterSettings {
    /// Stable MCP server name written into client config files.
    server_name: String,
    /// Default local streamable HTTP MCP URL used by most clients.
    default_http_url: String,
    /// Default HTTPS MCP URL used by bridge-based clients.
    default_https_url: String,
}

impl Default for IntegrationWriterSettings {
    fn default() -> Self {
        Self {
            server_name: "mentisdb".to_owned(),
            default_http_url: "http://127.0.0.1:9471".to_owned(),
            default_https_url: "https://my.mentisdb.com:9473".to_owned(),
        }
    }
}

impl IntegrationWriterSettings {
    pub(crate) fn with_url_for(
        &self,
        integration: IntegrationKind,
        url: impl Into<String>,
    ) -> Self {
        let mut next = self.clone();
        let url = url.into();
        match integration {
            IntegrationKind::ClaudeDesktop => next.default_https_url = url,
            _ => next.default_http_url = url,
        }
        next
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.server_name
    }

    pub(crate) fn url_for(&self, integration: IntegrationKind) -> &str {
        match integration {
            IntegrationKind::ClaudeDesktop => &self.default_https_url,
            _ => &self.default_http_url,
        }
    }
}

/// Fully expanded file-write plan for one integration target.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct IntegrationApplyPlan {
    /// Integration that will be updated.
    pub(crate) integration: IntegrationKind,
    /// Platform-specific path mapping used for the plan.
    pub(crate) platform: HostPlatform,
    /// Managed files to merge or create.
    pub(crate) files: Vec<ManagedFile>,
}

impl IntegrationApplyPlan {
    pub(crate) fn new(integration: IntegrationKind, platform: HostPlatform) -> Self {
        Self {
            integration,
            platform,
            files: Vec::new(),
        }
    }

    pub(crate) fn with_file(mut self, file: ManagedFile) -> Self {
        self.files.push(file);
        self
    }
}
