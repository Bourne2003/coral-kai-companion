pub mod client;
pub mod config;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use shared_types::{ActionResult, SoulError, SoulResult};

use crate::plugins::{CompanionPlugin, PluginContext};

pub use client::{McpClientManager, jsonrpc_stdio_request};
pub use config::{load_mcp_config_file, load_mcp_config_safe, parse_mcp_config_str};

/// Wraps [`McpClientManager`] as a [`CompanionPlugin`] (`name: "mcp_bridge"`, `builtin: false`)
/// exposing discovered `mcp:<server>:<tool>` actions only when `config.enabled == true`.
#[derive(Debug)]
pub struct McpPluginBridge {
    manager: Arc<McpClientManager>,
}

impl McpPluginBridge {
    /// Creates a new MCP plugin bridge wrapping `manager`.
    pub fn new(manager: Arc<McpClientManager>) -> Self {
        Self { manager }
    }

    /// Returns a reference to the underlying [`McpClientManager`].
    pub fn manager(&self) -> &Arc<McpClientManager> {
        &self.manager
    }
}

#[async_trait]
impl CompanionPlugin for McpPluginBridge {
    fn name(&self) -> &'static str {
        "mcp_bridge"
    }

    fn description(&self) -> &'static str {
        "Optional external MCP client bridge exposing configured server tools (disabled by default)."
    }

    fn builtin(&self) -> bool {
        false
    }

    fn supported_actions(&self) -> Vec<String> {
        if !self.manager.config().enabled {
            return Vec::new();
        }
        self.manager
            .list_tools()
            .into_iter()
            .map(|t| format!("mcp:{}:{}", t.server_name, t.tool_name))
            .collect()
    }

    async fn execute(
        &self,
        action: &str,
        params: &Value,
        _ctx: &PluginContext,
    ) -> SoulResult<ActionResult> {
        if !self.manager.config().enabled {
            return Err(SoulError::InvalidInput(
                "MCP client integration is disabled".to_string(),
            ));
        }

        let Some(rest) = action.strip_prefix("mcp:") else {
            return Err(SoulError::InvalidInput(format!(
                "Invalid MCP action format '{action}', expected mcp:<server>:<tool>"
            )));
        };
        let Some((server_name, tool_name)) = rest.split_once(':') else {
            return Err(SoulError::InvalidInput(format!(
                "Invalid MCP action format '{action}', expected mcp:<server>:<tool>"
            )));
        };

        let result = self
            .manager
            .call_tool(server_name, tool_name, params.clone())
            .await?;

        Ok(ActionResult {
            action: action.to_string(),
            plugin_name: self.name().to_string(),
            success: true,
            summary: format!("Executed MCP tool '{tool_name}' on server '{server_name}'"),
            data: Some(result),
        })
    }
}
