use std::process::Stdio;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shared_types::{McpConfig, McpServerConfig, McpToolInfo, SoulError, SoulResult};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;

const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
const SANDBOX_ENV_KEYS: &[&str] = &["PATH", "HOME", "LANG"];

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

/// Pure-Rust optional MCP client manager supporting sandboxed `stdio` and raw-TCP `sse`
/// JSON-RPC 2.0 transports with strict timeouts and graceful failure degradation.
#[derive(Debug, Default)]
pub struct McpClientManager {
    config: RwLock<McpConfig>,
    tools: RwLock<Vec<McpToolInfo>>,
    next_id: AtomicU64,
}

impl McpClientManager {
    /// Creates a manager with the given [`McpConfig`] (disabled by default via `Default`).
    pub fn new(config: McpConfig) -> Self {
        Self {
            config: RwLock::new(config),
            tools: RwLock::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Returns a snapshot of the current [`McpConfig`].
    pub fn config(&self) -> McpConfig {
        self.config.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Updates the active [`McpConfig`], clearing cached tools when MCP is disabled.
    pub fn set_config(&self, config: McpConfig) {
        let disabled = !config.enabled;
        if let Ok(mut guard) = self.config.write() {
            *guard = config;
        }
        if disabled && let Ok(mut tools) = self.tools.write() {
            tools.clear();
        }
    }

    /// Returns the most recently discovered MCP tools (empty when disabled).
    pub fn list_tools(&self) -> Vec<McpToolInfo> {
        if !self.config().enabled {
            return Vec::new();
        }
        self.tools.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Discovers tools across all enabled MCP servers, logging warnings and skipping any
    /// server that fails to spawn, connect, respond in time, or return valid JSON-RPC.
    pub async fn refresh_tools(&self) -> Vec<McpToolInfo> {
        let cfg = self.config();
        if !cfg.enabled {
            if let Ok(mut tools) = self.tools.write() {
                tools.clear();
            }
            return Vec::new();
        }

        let mut discovered = Vec::new();
        for server in cfg.servers.iter().filter(|s| s.enabled) {
            match self.fetch_server_tools(server).await {
                Ok(mut server_tools) => discovered.append(&mut server_tools),
                Err(err) => {
                    tracing::warn!(
                        server = %server.name,
                        transport = %server.transport,
                        error = %err,
                        "MCP tool discovery failed; skipping server safely"
                    );
                }
            }
        }

        if let Ok(mut guard) = self.tools.write() {
            *guard = discovered.clone();
        }
        discovered
    }

    /// Invokes a named MCP tool on a configured server, logging warnings on any failure.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Value,
    ) -> SoulResult<Value> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(SoulError::InvalidInput(
                "MCP client integration is disabled".into(),
            ));
        }

        let server = cfg
            .servers
            .into_iter()
            .find(|s| s.name == server_name && s.enabled)
            .ok_or_else(|| {
                SoulError::InvalidInput(format!(
                    "MCP server '{server_name}' is not configured or not enabled"
                ))
            })?;

        let params = json!({
            "name": tool_name,
            "arguments": arguments,
        });

        let res = self.send_mcp_request(&server, "tools/call", params).await;
        if let Err(ref err) = res {
            tracing::warn!(
                server = %server_name,
                tool = %tool_name,
                error = %err,
                "MCP tool call failed"
            );
        }
        res
    }

    async fn fetch_server_tools(&self, server: &McpServerConfig) -> SoulResult<Vec<McpToolInfo>> {
        let result = self
            .send_mcp_request(server, "tools/list", json!({}))
            .await?;

        let tools_arr = result
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                SoulError::Serialization("MCP tools/list response missing 'tools' array".into())
            })?;

        let mut parsed = Vec::with_capacity(tools_arr.len());
        for item in tools_arr {
            let Some(tool_name) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            parsed.push(McpToolInfo {
                server_name: server.name.clone(),
                tool_name: tool_name.to_string(),
                description,
            });
        }
        Ok(parsed)
    }

    async fn send_mcp_request(
        &self,
        server: &McpServerConfig,
        method: &str,
        params: Value,
    ) -> SoulResult<Value> {
        let req_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        match server.transport.trim().to_ascii_lowercase().as_str() {
            "stdio" => stdio_mcp_session_request(server, req_id, method, params).await,
            "sse" | "http" => sse_http_request(server, req_id, method, params).await,
            other => Err(SoulError::InvalidInput(format!(
                "Unsupported MCP transport '{other}' for server '{}'",
                server.name
            ))),
        }
    }
}

/// Executes a single sandboxed `stdio` JSON-RPC 2.0 round-trip with `env_clear()`
/// (preserving only `PATH`, `HOME`, `LANG`), `kill_on_drop(true)`, and strict timeout.
pub async fn jsonrpc_stdio_request(
    server: &McpServerConfig,
    method: &str,
    params: Value,
) -> SoulResult<Value> {
    let timeout = effective_timeout(server.timeout_ms);
    let mut child = spawn_sandboxed_stdio(server)?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| SoulError::Io("Failed to open child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SoulError::Io("Failed to open child stdout".into()))?;

    let req_id = 1_u64;
    let payload = serde_json::to_string(&JsonRpcRequest {
        jsonrpc: "2.0",
        id: req_id,
        method,
        params,
    })?;

    let io_future = async {
        stdin.write_all(payload.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        let mut reader = BufReader::new(stdout);
        read_jsonrpc_response_line(&mut reader, req_id).await
    };

    let res = match tokio::time::timeout(timeout, io_future).await {
        Ok(inner) => inner,
        Err(_) => {
            tracing::warn!(
                server = %server.name,
                method = %method,
                timeout_ms = timeout.as_millis() as u64,
                "MCP stdio JSON-RPC request timed out"
            );
            Err(SoulError::Io(format!(
                "MCP stdio request '{}' timed out after {} ms",
                method,
                timeout.as_millis()
            )))
        }
    };

    drop(stdin);
    let _ = child.kill().await;
    res
}

async fn stdio_mcp_session_request(
    server: &McpServerConfig,
    req_id: u64,
    method: &str,
    params: Value,
) -> SoulResult<Value> {
    let timeout = effective_timeout(server.timeout_ms);
    let mut child = spawn_sandboxed_stdio(server)?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| SoulError::Io("Failed to open child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SoulError::Io("Failed to open child stdout".into()))?;

    let session_future = async {
        let mut reader = BufReader::new(stdout);

        let init_id = req_id.saturating_mul(10);
        let init_req = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id: init_id,
            method: "initialize",
            params: json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "soulmate2d",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        })?;
        stdin.write_all(init_req.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        let _ = read_jsonrpc_response_line(&mut reader, init_id).await?;

        let initialized_notif = serde_json::to_string(&JsonRpcNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized",
            params: json!({}),
        })?;
        stdin.write_all(initialized_notif.as_bytes()).await?;
        stdin.write_all(b"\n").await?;

        let call_id = init_id + 1;
        let call_req = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id: call_id,
            method,
            params,
        })?;
        stdin.write_all(call_req.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        read_jsonrpc_response_line(&mut reader, call_id).await
    };

    let res = match tokio::time::timeout(timeout, session_future).await {
        Ok(inner) => inner,
        Err(_) => {
            tracing::warn!(
                server = %server.name,
                method = %method,
                timeout_ms = timeout.as_millis() as u64,
                "MCP stdio session timed out"
            );
            Err(SoulError::Io(format!(
                "MCP stdio request '{}' timed out after {} ms",
                method,
                timeout.as_millis()
            )))
        }
    };

    drop(stdin);
    let _ = child.kill().await;
    res
}

fn spawn_sandboxed_stdio(server: &McpServerConfig) -> SoulResult<tokio::process::Child> {
    let program = server
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            SoulError::InvalidInput(format!(
                "MCP stdio server '{}' has no command configured",
                server.name
            ))
        })?;

    let mut cmd = Command::new(program);
    cmd.args(&server.args)
        .env_clear()
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in SANDBOX_ENV_KEYS {
        if let Some(val) = std::env::var_os(key) {
            cmd.env(key, val);
        }
    }

    cmd.spawn().map_err(|err| {
        tracing::warn!(
            server = %server.name,
            command = %program,
            error = %err,
            "Failed to spawn sandboxed MCP stdio process"
        );
        SoulError::Io(format!("Failed to spawn MCP server '{}': {err}", server.name))
    })
}

async fn read_jsonrpc_response_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    expected_id: u64,
) -> SoulResult<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Err(SoulError::Io(
                "MCP stdio server closed stdout before returning JSON-RPC response".into(),
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) else {
            continue;
        };

        let id_matches = resp
            .id
            .as_ref()
            .and_then(|v| v.as_u64())
            .is_some_and(|id| id == expected_id);
        if resp.id.is_some() && !id_matches {
            continue;
        }

        if let Some(err) = resp.error {
            return Err(SoulError::Internal(format!(
                "MCP JSON-RPC error {}: {}",
                err.code, err.message
            )));
        }
        return Ok(resp.result.unwrap_or(Value::Null));
    }
}

async fn sse_http_request(
    server: &McpServerConfig,
    req_id: u64,
    method: &str,
    params: Value,
) -> SoulResult<Value> {
    let raw_url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            SoulError::InvalidInput(format!(
                "MCP sse server '{}' has no URL configured",
                server.name
            ))
        })?;

    let (host, port, path) = parse_http_url(raw_url)?;
    let timeout = effective_timeout(server.timeout_ms);
    let body = serde_json::to_string(&JsonRpcRequest {
        jsonrpc: "2.0",
        id: req_id,
        method,
        params,
    })?;

    let host_header = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };

    let http_req = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let io_future = async {
        let mut stream = TcpStream::connect((host.as_str(), port)).await?;
        stream.write_all(http_req.as_bytes()).await?;
        stream.flush().await?;

        let mut raw_response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if raw_response.len() + n > MAX_HTTP_RESPONSE_BYTES {
                return Err(SoulError::Io(
                    "MCP HTTP/SSE response exceeded 1 MiB safety limit".into(),
                ));
            }
            raw_response.extend_from_slice(&buf[..n]);
        }

        parse_http_jsonrpc_or_sse(&raw_response, req_id)
    };

    match tokio::time::timeout(timeout, io_future).await {
        Ok(inner) => inner,
        Err(_) => {
            tracing::warn!(
                server = %server.name,
                url = %raw_url,
                timeout_ms = timeout.as_millis() as u64,
                "MCP HTTP/SSE request timed out"
            );
            Err(SoulError::Io(format!(
                "MCP SSE request timed out after {} ms",
                timeout.as_millis()
            )))
        }
    }
}

fn parse_http_url(raw_url: &str) -> SoulResult<(String, u16, String)> {
    let rest = raw_url.strip_prefix("http://").ok_or_else(|| {
        SoulError::InvalidInput(format!(
            "Unsupported MCP SSE URL '{raw_url}': only plain http:// is supported offline"
        ))
    })?;

    let (authority, path) = match rest.split_once('/') {
        Some((auth, tail)) => (auth, format!("/{tail}")),
        None => (rest, "/".to_string()),
    };

    if authority.is_empty() {
        return Err(SoulError::InvalidInput("Empty host in MCP URL".into()));
    }

    let (host, port) = if let Some((h, p)) = authority.rsplit_once(':') {
        let parsed_port = p
            .parse::<u16>()
            .map_err(|_| SoulError::InvalidInput(format!("Invalid port in MCP URL: {p}")))?;
        (h.to_string(), parsed_port)
    } else {
        (authority.to_string(), 80)
    };

    Ok((host, port, path))
}

fn parse_http_jsonrpc_or_sse(raw: &[u8], expected_id: u64) -> SoulResult<Value> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|idx| (idx, 4usize))
        .or_else(|| {
            raw.windows(2)
                .position(|w| w == b"\n\n")
                .map(|idx| (idx, 2usize))
        })
        .ok_or_else(|| SoulError::Serialization("Malformed HTTP response from MCP server".into()))?;

    let headers_str = std::str::from_utf8(&raw[..split.0])
        .map_err(|_| SoulError::Serialization("Non-UTF8 HTTP headers".into()))?;
    let body_bytes = &raw[split.0 + split.1..];

    let mut lines = headers_str.lines();
    let status_line = lines.next().unwrap_or("");
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| SoulError::Serialization(format!("Invalid HTTP status: {status_line}")))?;

    if !(200..300).contains(&status_code) {
        return Err(SoulError::Io(format!(
            "MCP HTTP server returned status {status_code}"
        )));
    }

    let mut is_chunked = false;
    let mut is_sse = false;
    for header in lines {
        if let Some((name, value)) = header.split_once(':') {
            let name_l = name.trim().to_ascii_lowercase();
            let val_l = value.trim().to_ascii_lowercase();
            if name_l == "transfer-encoding" && val_l.contains("chunked") {
                is_chunked = true;
            }
            if name_l == "content-type" && val_l.contains("text/event-stream") {
                is_sse = true;
            }
        }
    }

    let decoded_body = if is_chunked {
        decode_http_chunked(body_bytes)?
    } else {
        body_bytes.to_vec()
    };

    let body_str = std::str::from_utf8(&decoded_body)
        .map_err(|_| SoulError::Serialization("Non-UTF8 MCP response body".into()))?;

    if is_sse || body_str.lines().any(|l| l.starts_with("data:")) {
        return extract_sse_jsonrpc(body_str, expected_id);
    }

    let resp: JsonRpcResponse = serde_json::from_str(body_str.trim())?;
    if let Some(err) = resp.error {
        return Err(SoulError::Internal(format!(
            "MCP JSON-RPC error {}: {}",
            err.code, err.message
        )));
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

fn decode_http_chunked(mut bytes: &[u8]) -> SoulResult<Vec<u8>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let Some(pos) = bytes.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_line = std::str::from_utf8(&bytes[..pos])
            .map_err(|_| SoulError::Serialization("Invalid chunk size header".into()))?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_line, 16)
            .map_err(|_| SoulError::Serialization("Invalid hex chunk size".into()))?;
        bytes = &bytes[pos + 2..];
        if size == 0 {
            break;
        }
        if bytes.len() < size {
            return Err(SoulError::Serialization("Truncated HTTP chunk".into()));
        }
        out.extend_from_slice(&bytes[..size]);
        bytes = &bytes[size..];
        if bytes.starts_with(b"\r\n") {
            bytes = &bytes[2..];
        }
    }
    Ok(out)
}

fn extract_sse_jsonrpc(body: &str, expected_id: u64) -> SoulResult<Value> {
    let mut event_data = String::new();
    let mut parsed_candidates = Vec::new();

    for line in body.lines().chain(std::iter::once("")) {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            if !event_data.is_empty() {
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&event_data) {
                    parsed_candidates.push(resp);
                }
                event_data.clear();
            }
            continue;
        }
        if let Some(data) = trimmed.strip_prefix("data:") {
            if !event_data.is_empty() {
                event_data.push('\n');
            }
            event_data.push_str(data.trim_start());
        }
    }

    for resp in parsed_candidates {
        let id_ok = resp
            .id
            .as_ref()
            .and_then(|v| v.as_u64())
            .is_none_or(|id| id == expected_id);
        if id_ok {
            if let Some(err) = resp.error {
                return Err(SoulError::Internal(format!(
                    "MCP SSE JSON-RPC error {}: {}",
                    err.code, err.message
                )));
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    Err(SoulError::Serialization(
        "No matching JSON-RPC payload found in MCP SSE stream".into(),
    ))
}

fn effective_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        timeout_ms
    })
}
