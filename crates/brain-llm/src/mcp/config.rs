use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use shared_types::{McpConfig, McpServerConfig, SoulError, SoulResult};

const DEFAULT_SERVER_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStyleFile {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default, alias = "mcp_servers")]
    mcp_servers: BTreeMap<String, ClaudeStyleServer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStyleServer {
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    disabled: Option<bool>,
    #[serde(default, alias = "timeout_ms")]
    timeout_ms: Option<u64>,
}

/// Parses an external MCP JSON string supporting both native [`McpConfig`]
/// and Claude Desktop / standard `mcpServers` JSON schemas.
pub fn parse_mcp_config_str(raw: &str, source_path: Option<&Path>) -> SoulResult<McpConfig> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(SoulError::Serialization("MCP config JSON is empty".into()));
    }

    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    let obj = value
        .as_object()
        .ok_or_else(|| SoulError::Serialization("MCP config root must be a JSON object".into()))?;

    let path_str = source_path.map(|p| p.to_string_lossy().into_owned());

    if obj.contains_key("servers") {
        let mut cfg: McpConfig = serde_json::from_value(value)?;
        if cfg.config_path.is_none() {
            cfg.config_path = path_str;
        }
        for server in &mut cfg.servers {
            if server.timeout_ms == 0 {
                server.timeout_ms = DEFAULT_SERVER_TIMEOUT_MS;
            }
            if server.transport.trim().is_empty() {
                server.transport = if server.url.is_some() {
                    "sse".to_string()
                } else {
                    "stdio".to_string()
                };
            }
        }
        return Ok(cfg);
    }

    if obj.contains_key("mcpServers") || obj.contains_key("mcp_servers") {
        let claude: ClaudeStyleFile = serde_json::from_value(value)?;
        let servers = claude
            .mcp_servers
            .into_iter()
            .map(|(name, entry)| {
                let transport = entry
                    .transport
                    .map(|t| t.trim().to_ascii_lowercase())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        if entry.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
                            "sse".to_string()
                        } else {
                            "stdio".to_string()
                        }
                    });
                let enabled = entry
                    .enabled
                    .unwrap_or_else(|| !entry.disabled.unwrap_or(false));
                let timeout_ms = entry
                    .timeout_ms
                    .filter(|&ms| ms > 0)
                    .unwrap_or(DEFAULT_SERVER_TIMEOUT_MS);

                McpServerConfig {
                    name,
                    transport,
                    command: entry.command,
                    args: entry.args,
                    url: entry.url,
                    enabled,
                    timeout_ms,
                }
            })
            .collect();

        return Ok(McpConfig {
            enabled: claude.enabled.unwrap_or(false),
            config_path: path_str,
            servers,
        });
    }

    Err(SoulError::Serialization(
        "Unrecognized MCP JSON config: expected 'servers' or 'mcpServers'".into(),
    ))
}

/// Reads and parses an MCP config JSON file safely.
pub fn load_mcp_config_file(path: &Path) -> SoulResult<McpConfig> {
    let content = std::fs::read_to_string(path)?;
    parse_mcp_config_str(&content, Some(path))
}

/// Reads an MCP config JSON file, logging a warning and falling back to disabled
/// [`McpConfig::default()`] if the file is missing, unreadable, or malformed.
pub fn load_mcp_config_safe(path: &Path) -> McpConfig {
    match load_mcp_config_file(path) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "Failed to load MCP config file; falling back to disabled default"
            );
            McpConfig::default()
        }
    }
}
