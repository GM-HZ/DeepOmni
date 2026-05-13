//! Server protocol types — host-safe notification/request projections.
//! Pattern from Codex App Server: EventFrame → ServerNotification/ServerRequest.

use crate::id::{ThreadId, TurnId};
use serde::{Deserialize, Serialize};

/// Fire-and-forget notification sent to connected clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerNotification {
    TurnStarted {
        thread_id: ThreadId,
        turn_id: TurnId,
        user_input: String,
    },
    AssistantDelta {
        thread_id: ThreadId,
        turn_id: TurnId,
        delta: String,
    },
    TurnCompleted {
        thread_id: ThreadId,
        turn_id: TurnId,
    },
    TurnFailed {
        thread_id: ThreadId,
        turn_id: TurnId,
        error: String,
    },
    ToolCallCompleted {
        thread_id: ThreadId,
        turn_id: TurnId,
        tool_name: String,
        success: bool,
    },
    Warning {
        thread_id: ThreadId,
        message: String,
    },
}

/// Request that requires a client response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerRequest {
    ApprovalNeeded {
        thread_id: ThreadId,
        turn_id: TurnId,
        approval_id: String,
        tool_name: String,
        reason: String,
    },
}

/// Client response to a server request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientResponse {
    ApprovalDecision {
        thread_id: ThreadId,
        approval_id: String,
        approved: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_serializes() {
        let n = ServerNotification::TurnStarted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("tu1"),
            user_input: "hello".into(),
        };
        let json = serde_json::to_value(&n).unwrap();
        assert_eq!(json["type"], "turn_started");
    }

    #[test]
    fn test_request_serializes() {
        let r = ServerRequest::ApprovalNeeded {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("tu1"),
            approval_id: "a1".into(),
            tool_name: "write_file".into(),
            reason: "mutating".into(),
        };
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["type"], "approval_needed");
    }
}
