//! Turn types — a single user→agent round-trip within a thread.

use serde::{Deserialize, Serialize};

use crate::id::{ThreadId, ToolCallId, TurnId};

/// Status of a turn within its lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Started,
    Streaming,
    WaitingForApproval,
    ExecutingTool,
    Completed,
    Failed,
    Interrupted,
}

/// A structured item within a turn's transcript. Stored durably so the
/// full turn can be reconstructed for continuation, replay, and approval resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnItem {
    UserMessage {
        content: String,
    },
    AssistantDelta {
        delta: String,
    },
    AssistantCompleted,
    ReasoningBlock {
        content: String,
        replay_required: bool,
    },
    ToolCall {
        call_id: ToolCallId,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        call_id: ToolCallId,
        tool_name: String,
        output_preview: Option<String>,
        success: bool,
    },
    ApprovalDecision {
        call_id: ToolCallId,
        approved: bool,
    },
}

/// A turn within a thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub thread_id: ThreadId,
    pub status: TurnStatus,
    pub user_input: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
}

/// Request to start a new turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTurnRequest {
    pub input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<TurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_token_budget: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ThreadId;

    #[test]
    fn test_turn_status_serde() {
        let json = r#""streaming""#;
        let s: TurnStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s, TurnStatus::Streaming);
    }

    #[test]
    fn test_create_turn_request_minimal() {
        let req = CreateTurnRequest {
            input: "hello".into(),
            model: None,
            parent_turn_id: None,
            subagent_id: None,
            max_token_budget: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: CreateTurnRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.input, "hello");
    }

    #[test]
    fn test_turn_json_roundtrip() {
        let turn = Turn {
            id: TurnId::new(),
            thread_id: ThreadId::from_string("thread-abc"),
            status: TurnStatus::Started,
            user_input: "Fix the test".into(),
            created_at: 1700000000,
            completed_at: None,
            model: Some("deepseek-chat".into()),
            model_provider: None,
            parent_turn_id: None,
            parent_thread_id: None,
            subagent_id: None,
        };
        let json = serde_json::to_string(&turn).unwrap();
        let parsed: Turn = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.thread_id.as_str(), "thread-abc");
        assert_eq!(parsed.status, TurnStatus::Started);
    }
}
