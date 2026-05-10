//! Tool schema, invocation, output, and spec types.
//!
//! Adapted from Codex `ToolHandler`/`ToolSpec` and DeepSeek-TUI `ToolInvocation`/`ToolOutput`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::ToolCallId;

/// Typed payload for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolPayload {
    /// Standard function call with JSON arguments.
    Function { arguments: String },
    /// Free-form custom input.
    Custom { input: String },
    /// Local shell command invocation.
    LocalShell {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// MCP tool call routed through an MCP server.
    Mcp {
        server: String,
        tool: String,
        raw_arguments: Value,
    },
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolOutput {
    Function {
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<Value>,
        success: bool,
    },
    Mcp {
        result: Value,
    },
}

/// A validated tool invocation ready for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub payload: ToolPayload,
}

/// JSON Schema for a tool's input parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Fully configured tool spec as exposed to models.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum ToolSpec {
    #[serde(rename = "function")]
    Function(ToolSpecDetails),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpecDetails {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_payload_function_json() {
        let payload = ToolPayload::Function {
            arguments: r#"{"path":"/src/main.rs"}"#.into(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("function"));
        assert!(json.contains("arguments"));
    }

    #[test]
    fn test_tool_output_function_json() {
        let output = ToolOutput::Function {
            body: Some(serde_json::json!({"result": "ok"})),
            success: true,
        };
        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains(r#""success":true"#));
    }

    #[test]
    fn test_tool_invocation_roundtrip() {
        let inv = ToolInvocation {
            call_id: ToolCallId::new(),
            tool_name: "read_file".into(),
            payload: ToolPayload::Function {
                arguments: r#"{"path":"/tmp/test.txt"}"#.into(),
            },
        };
        let json = serde_json::to_string(&inv).unwrap();
        let parsed: ToolInvocation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "read_file");
    }
}
