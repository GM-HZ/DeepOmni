//! Runtime event protocol.
//!
//! Based on PRD §9. Every externally visible state change emits a typed
//! event persisted with a monotonic sequence number.

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};

/// A typed frame representing one atom of observable state change.
///
/// Events are per-thread, monotonic, and replayable via `since_seq`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventFrame {
    // ── Thread lifecycle ──
    ThreadCreated {
        thread_id: ThreadId,
        workspace: String,
    },
    ThreadUpdated {
        thread_id: ThreadId,
    },
    ThreadArchived {
        thread_id: ThreadId,
    },

    // ── Turn lifecycle ──
    TurnStarted {
        thread_id: ThreadId,
        turn_id: TurnId,
        user_input: String,
    },
    TurnSteered {
        thread_id: ThreadId,
        turn_id: TurnId,
        steering_input: String,
    },
    TurnInterrupted {
        thread_id: ThreadId,
        turn_id: TurnId,
    },

    // ── Assistant output ──
    AssistantMessageDelta {
        turn_id: TurnId,
        message_id: MessageId,
        delta: String,
    },
    AssistantMessageCompleted {
        turn_id: TurnId,
        message_id: MessageId,
    },

    // ── Reasoning (DeepSeek-specific, host-renderable) ──
    AssistantReasoningDelta {
        turn_id: TurnId,
        message_id: MessageId,
        delta: String,
        /// Whether the provider requires this reasoning for later replay.
        replay_required: bool,
        /// The host may choose not to display reasoning.
        visibility: ReasoningVisibility,
    },
    AssistantReasoningCompleted {
        turn_id: TurnId,
        message_id: MessageId,
        replay_required: bool,
    },

    // ── Tool call lifecycle ──
    ToolCallStarted {
        turn_id: TurnId,
        call_id: ToolCallId,
        tool_name: String,
    },
    ToolCallArgumentsDelta {
        turn_id: TurnId,
        call_id: ToolCallId,
        delta: String,
    },
    ToolCallRequiresApproval {
        turn_id: TurnId,
        call_id: ToolCallId,
        tool_name: String,
        reason: String,
    },
    ToolCallApproved {
        turn_id: TurnId,
        call_id: ToolCallId,
    },
    ToolCallRejected {
        turn_id: TurnId,
        call_id: ToolCallId,
    },
    ToolCallCompleted {
        turn_id: TurnId,
        call_id: ToolCallId,
        tool_name: String,
        success: bool,
        /// Summary of tool output (may be truncated).
        output_preview: Option<String>,
    },
    ToolCallFailed {
        turn_id: TurnId,
        call_id: ToolCallId,
        error: String,
    },

    // ── Context management ──
    ContextCompactionStarted {
        turn_id: TurnId,
    },
    ContextCompactionCompleted {
        turn_id: TurnId,
    },

    // ── Sub-agents ──
    SubagentSpawned {
        parent_turn_id: TurnId,
        subagent_id: SubagentId,
        task: String,
    },
    SubagentCompleted {
        parent_turn_id: TurnId,
        subagent_id: SubagentId,
        result_summary: String,
    },
    SubagentFailed {
        parent_turn_id: TurnId,
        subagent_id: SubagentId,
        error: String,
    },

    // ── Terminal states ──
    TurnCompleted {
        turn_id: TurnId,
    },
    TurnFailed {
        turn_id: TurnId,
        error: String,
    },

    // ── Ad-hoc ──
    RuntimeWarning {
        message: String,
    },
}

/// An event frame with its durable sequence number.
///
/// The seq is assigned by StateStore on persistence. Every consumer
/// (SSE, SDK, replay) uses this envelope as the smallest unit of event flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: i64,
    #[serde(flatten)]
    pub frame: EventFrame,
}

/// Controls how a reasoning event is presented to hosts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningVisibility {
    /// Host may display the reasoning inline.
    HostRenderable,
    /// Host should not display but must not discard.
    ReplayOnly,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_json_snapshot_turn_started() {
        let event = EventFrame::TurnStarted {
            thread_id: ThreadId::from_string("thread-1"),
            turn_id: TurnId::from_string("turn-1"),
            user_input: "hello".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EventFrame = serde_json::from_str(&json).unwrap();
        match parsed {
            EventFrame::TurnStarted {
                thread_id,
                turn_id,
                user_input,
            } => {
                assert_eq!(thread_id.as_str(), "thread-1");
                assert_eq!(turn_id.as_str(), "turn-1");
                assert_eq!(user_input, "hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_reasoning_event_json() {
        let event = EventFrame::AssistantReasoningDelta {
            turn_id: TurnId::from_string("turn-2"),
            message_id: MessageId::from_string("msg-1"),
            delta: "Let me think...".into(),
            replay_required: true,
            visibility: ReasoningVisibility::HostRenderable,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("reasoning"));
        assert!(json.contains("replay_required"));
    }

    #[test]
    fn test_subagent_event_json() {
        let event = EventFrame::SubagentSpawned {
            parent_turn_id: TurnId::from_string("turn-3"),
            subagent_id: SubagentId::from_string("subagent-1"),
            task: "Review this file".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EventFrame = serde_json::from_str(&json).unwrap();
        match parsed {
            EventFrame::SubagentSpawned {
                parent_turn_id,
                subagent_id,
                task,
            } => {
                assert_eq!(parent_turn_id.as_str(), "turn-3");
                assert_eq!(subagent_id.as_str(), "subagent-1");
                assert_eq!(task, "Review this file");
            }
            _ => panic!("wrong variant"),
        }
    }
}
