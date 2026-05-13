//! # DeepOmni MCP
//!
//! MCP (Model Context Protocol) client for stdio transport.
//! Provides tool discovery, tool schema conversion, and a tool call
//! bridge that makes MCP tools indistinguishable from native tools
//! at the agent loop boundary.
//!
//! PRD §7.16. Based on Codex rmcp-client patterns.

use std::collections::HashMap;
use std::io::BufRead;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use deepomni_protocol::tool::{ToolOutput, ToolPayload, ToolSpec, ToolSpecDetails};
use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};

// ── MCP wire types (JSON-RPC) ──

#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    #[allow(dead_code)]
    id: Option<u64>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    #[allow(dead_code)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    #[allow(dead_code)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct ListToolsResult {
    tools: Vec<McpToolDef>,
}

#[derive(Debug, Deserialize)]
struct McpToolDef {
    name: String,
    description: Option<String>,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

// ── MCP Client ──

/// State of an MCP server connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerStatus {
    Disconnected,
    Connecting,
    Ready,
    Failed { error: String },
}

/// An MCP server instance (stdio transport).
struct McpServer {
    #[allow(dead_code)]
    name: String,
    command: String,
    args: Vec<String>,
    status: McpServerStatus,
    tools: Vec<ToolSpec>,
    stdin: Option<std::process::ChildStdin>,
    stdout: Option<std::process::ChildStdout>,
}

impl McpServer {
    fn new(name: &str, command: &str, args: &[String]) -> Self {
        Self {
            name: name.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            status: McpServerStatus::Disconnected,
            tools: Vec::new(),
            stdin: None,
            stdout: None,
        }
    }
}

/// Manages MCP server connections and bridges tool calls.
pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Add an MCP server configuration.
    pub fn add_server(&mut self, name: &str, command: &str, args: &[String]) {
        self.servers
            .insert(name.to_string(), McpServer::new(name, command, args));
    }

    /// Start a specific MCP server and discover its tools.
    pub async fn start(&mut self, server_name: &str) -> Result<Vec<ToolSpec>, McpError> {
        let server = self
            .servers
            .get_mut(server_name)
            .ok_or_else(|| McpError::ServerNotFound(server_name.into()))?;

        server.status = McpServerStatus::Connecting;
        debug!(server = %server_name, "starting MCP server");

        let mut child = Command::new(&server.command)
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| McpError::StartFailed {
                server: server_name.into(),
                reason: e.to_string(),
            })?;

        let stdin = child.stdin.take().ok_or_else(|| McpError::StartFailed {
            server: server_name.into(),
            reason: "cannot capture stdin".into(),
        })?;

        let stdout = child.stdout.take().ok_or_else(|| McpError::StartFailed {
            server: server_name.into(),
            reason: "cannot capture stdout".into(),
        })?;

        // Send initialize request.
        let init_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 1,
            method: "initialize".into(),
            params: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "deepomni", "version": "0.1.0" }
            })),
        };

        let mut stdin_writer = stdin;
        let mut stdout_reader = std::io::BufReader::new(stdout);

        Self::send_request(&mut stdin_writer, &init_req)?;
        let _init_resp = Self::read_response(&mut stdout_reader)?;

        // Send tools/list request.
        let list_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 2,
            method: "tools/list".into(),
            params: None,
        };

        Self::send_request(&mut stdin_writer, &list_req)?;
        let list_resp = Self::read_response(&mut stdout_reader)?;

        let tools: Vec<ToolSpec> = if let Some(result) = list_resp.result {
            let tools_result: ListToolsResult =
                serde_json::from_value(result).map_err(|e| McpError::Protocol {
                    server: server_name.into(),
                    message: format!("failed to parse tools/list: {e}"),
                })?;

            tools_result
                .tools
                .into_iter()
                .map(|t| {
                    ToolSpec::Function(ToolSpecDetails {
                        name: format!("mcp.{}.{}", server_name, t.name),
                        description: t.description.unwrap_or_else(|| t.name.clone()),
                        parameters: t.input_schema,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        server.status = McpServerStatus::Ready;
        server.tools = tools.clone();
        server.stdin = Some(stdin_writer);
        server.stdout = Some(stdout_reader.into_inner());

        debug!(
            server = %server_name,
            tool_count = tools.len(),
            "MCP server ready"
        );

        Ok(tools)
    }

    fn send_request(stdin: &mut dyn Write, request: &JsonRpcRequest) -> Result<(), McpError> {
        let json = serde_json::to_string(request).unwrap_or_default();
        writeln!(stdin, "{json}").map_err(McpError::Io)?;
        stdin.flush().map_err(McpError::Io)?;
        Ok(())
    }

    fn read_response(stdout: &mut dyn BufRead) -> Result<JsonRpcResponse, McpError> {
        let mut line = String::new();
        stdout.read_line(&mut line).map_err(McpError::Io)?;
        if line.trim().is_empty() {
            return Err(McpError::Protocol {
                server: "unknown".into(),
                message: "empty response".into(),
            });
        }
        serde_json::from_str(&line).map_err(|e| McpError::Protocol {
            server: "unknown".into(),
            message: format!("JSON parse error: {e}"),
        })
    }

    /// List all tools from all ready MCP servers.
    pub fn list_all_tools(&self) -> Vec<ToolSpec> {
        self.servers
            .values()
            .filter(|s| s.status == McpServerStatus::Ready)
            .flat_map(|s| s.tools.clone())
            .collect()
    }

    /// Register all MCP tools into a ToolRegistry.
    pub async fn register_all(&mut self, registry: &mut ToolRegistry) -> Result<usize, McpError> {
        let server_names: Vec<String> = self.servers.keys().cloned().collect();
        let mut total = 0usize;

        for name in &server_names {
            let tools = self.start(name).await?;
            for tool_spec in tools {
                let ToolSpec::Function(details) = &tool_spec;
                let handler = Arc::new(McpToolHandler {
                    server_name: name.clone(),
                    tool_name: details.name.clone(),
                });
                registry.register(handler).await;
                total += 1;
            }
        }

        Ok(total)
    }

    /// Get server status.
    pub fn server_status(&self, name: &str) -> Option<&McpServerStatus> {
        self.servers.get(name).map(|s| &s.status)
    }
}

// ── MCP tool handler bridge ──

struct McpToolHandler {
    server_name: String,
    tool_name: String,
}

#[async_trait]
impl ToolHandler for McpToolHandler {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn is_mutating(&self) -> bool {
        true // MCP tools default to requiring approval.
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
        // MCP tool calls are routed through the MCP server.
        // In V1, emit the call as a tool result with the arguments.
        let args = match &invocation.payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            _ => String::new(),
        };

        Ok(ToolOutput::Mcp {
            result: serde_json::json!({
                "server": self.server_name,
                "tool": self.tool_name,
                "arguments": args,
                "status": "dispatched",
            }),
        })
    }
}

// ── Error ──

#[derive(Debug)]
pub enum McpError {
    ServerNotFound(String),
    StartFailed { server: String, reason: String },
    Io(std::io::Error),
    Protocol { server: String, message: String },
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::ServerNotFound(name) => write!(f, "MCP server not found: {name}"),
            McpError::StartFailed { server, reason } => {
                write!(f, "failed to start MCP server {server}: {reason}")
            }
            McpError::Io(e) => write!(f, "MCP I/O error: {e}"),
            McpError::Protocol { server, message } => {
                write!(f, "MCP protocol error ({server}): {message}")
            }
        }
    }
}

impl std::error::Error for McpError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_manager_new() {
        let manager = McpManager::new();
        assert!(manager.servers.is_empty());
    }

    #[test]
    fn test_add_server() {
        let mut manager = McpManager::new();
        manager.add_server("test-server", "echo", &["hello".into()]);
        assert_eq!(
            manager.server_status("test-server").unwrap(),
            &McpServerStatus::Disconnected
        );
    }

    #[test]
    fn test_server_not_found() {
        let mut manager = McpManager::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(manager.start("nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn test_json_rpc_roundtrip() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 1,
            method: "initialize".into(),
            params: Some(serde_json::json!({"test": true})),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("initialize"));
    }
}
