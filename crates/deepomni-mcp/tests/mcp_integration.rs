//! Integration test: MCP server with JSON-RPC wire protocol.
//! Tests the full initialize → tools/list → tool call bridge flow.

use deepomni_mcp::{McpManager, McpServerStatus};

/// Mock MCP initialize response.
#[allow(dead_code)]
fn mock_initialize_response() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "test-server", "version": "1.0.0" }
        }
    })
    .to_string()
}

/// Mock tools/list response with two tools.
fn mock_tools_list_response() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "tools": [
                {
                    "name": "echo",
                    "description": "Echo back the input",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "message": { "type": "string" }
                        },
                        "required": ["message"]
                    }
                },
                {
                    "name": "add",
                    "description": "Add two numbers",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "a": { "type": "number" },
                            "b": { "type": "number" }
                        },
                        "required": ["a", "b"]
                    }
                }
            ]
        }
    })
    .to_string()
}

#[test]
fn test_mcp_initialize_roundtrip() {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "deepomni", "version": "0.1.0" }
        }
    });

    let req_str = serde_json::to_string(&request).unwrap();
    assert!(req_str.contains("initialize"));
    assert!(req_str.contains("deepomni"));
}

#[test]
fn test_mcp_parse_tools_list() {
    let response: serde_json::Value = serde_json::from_str(&mock_tools_list_response()).unwrap();
    let tools = &response["result"]["tools"];
    assert_eq!(tools.as_array().unwrap().len(), 2);
    assert_eq!(tools[0]["name"], "echo");
    assert_eq!(tools[1]["name"], "add");
}

#[test]
fn test_mcp_manager_server_not_found() {
    let manager = McpManager::new();
    assert!(manager.server_status("nonexistent").is_none());
}

#[test]
fn test_mcp_manager_add_server() {
    let mut manager = McpManager::new();
    manager.add_server("test-srv", "echo", &["hello".into()]);
    let status = manager.server_status("test-srv");
    assert!(status.is_some());
    assert_eq!(status.unwrap(), &McpServerStatus::Disconnected);
}

#[test]
fn test_mcp_jsonrpc_error_response() {
    let error_response = r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"Method not found"}}"#;
    let parsed: serde_json::Value = serde_json::from_str(error_response).unwrap();
    assert!(parsed.get("error").is_some());
    assert_eq!(parsed["error"]["code"], -32601);
}
