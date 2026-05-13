//! Runtime event protocol.
//!
//! Based on PRD §9. Every externally visible state change emits a typed
//! event persisted with a monotonic sequence number.

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};
use crate::op::SubmissionId;

/// Codex-style event envelope emitted by a session loop for one submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: SubmissionId,
    pub msg: EventMsg,
}

/// Message emitted on the session event queue. Corresponds to one
/// lifecycle state of a submission. Pattern from Codex: the event queue
/// carries the full lifecycle — started → in_progress → completed/failed
/// — and every event carries the correlated SubmissionId.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventMsg {
    /// The session loop accepted the submission and will begin processing.
    SubmissionStarted { thread_id: ThreadId },
    /// The handler has started executing the op (post-validation, pre-work).
    OpStarted { thread_id: ThreadId },
    /// The handler finished the op successfully with a result payload.
    OpCompleted {
        thread_id: ThreadId,
        turn_id: Option<TurnId>,
        status: String,
        user_input: String,
    },
    /// The handler encountered an error processing the op.
    OpFailed { thread_id: ThreadId, error: String },
    /// The session loop has finished all processing for this submission.
    SubmissionCompleted { thread_id: ThreadId },
    /// An event frame projected from the journal. The `frame` field is
    /// NOT flattened — the EventFrame already carries thread_id and turn_id,
    /// and flattening would produce duplicate keys on serialization.
    Frame { seq: i64, frame: EventFrame },
    /// The session event stream is permanently closed (session ended).
    StreamClosed { thread_id: ThreadId },
}

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

    #[test]
    fn test_session_event_serializes_with_submission_id() {
        let event = Event {
            id: SubmissionId("sub-1".into()),
            msg: EventMsg::SubmissionCompleted {
                thread_id: ThreadId::from_string("thread-1"),
            },
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["id"], "sub-1");
        assert_eq!(json["msg"]["type"], "submission_completed");
        assert_eq!(json["msg"]["thread_id"], "thread-1");
    }

    // ── EventMsg snapshot tests ──

    #[test]
    fn test_event_msg_submission_started_snapshot() {
        let msg = EventMsg::SubmissionStarted {
            thread_id: ThreadId::from_string("t1"),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "submission_started");
        assert_eq!(json["thread_id"], "t1");
    }

    #[test]
    fn test_event_msg_op_started_snapshot() {
        let msg = EventMsg::OpStarted {
            thread_id: ThreadId::from_string("t1"),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "op_started");
        assert_eq!(json["thread_id"], "t1");
    }

    #[test]
    fn test_event_msg_op_completed_snapshot() {
        let msg = EventMsg::OpCompleted {
            thread_id: ThreadId::from_string("t1"),
            turn_id: Some(TurnId::from_string("turn-1")),
            status: "completed".into(),
            user_input: "hello".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "op_completed");
        assert_eq!(json["thread_id"], "t1");
        assert_eq!(json["turn_id"], "turn-1");
        assert_eq!(json["status"], "completed");
    }

    #[test]
    fn test_event_msg_op_failed_snapshot() {
        let msg = EventMsg::OpFailed {
            thread_id: ThreadId::from_string("t1"),
            error: "something went wrong".into(),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "op_failed");
        assert_eq!(json["thread_id"], "t1");
        assert_eq!(json["error"], "something went wrong");
    }

    #[test]
    fn test_event_msg_submission_completed_snapshot() {
        let msg = EventMsg::SubmissionCompleted {
            thread_id: ThreadId::from_string("t1"),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "submission_completed");
        assert_eq!(json["thread_id"], "t1");
    }

    #[test]
    fn test_event_msg_frame_snapshot() {
        let msg = EventMsg::Frame {
            seq: 42,
            frame: EventFrame::TurnStarted {
                thread_id: ThreadId::from_string("t1"),
                turn_id: TurnId::from_string("turn-1"),
                user_input: "hi".into(),
            },
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "frame");
        assert_eq!(json["seq"], 42);
        assert_eq!(json["frame"]["event"], "turn_started");
        assert_eq!(json["frame"]["thread_id"], "t1");
    }

    #[test]
    fn test_event_msg_stream_closed_snapshot() {
        let msg = EventMsg::StreamClosed {
            thread_id: ThreadId::from_string("t1"),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "stream_closed");
        assert_eq!(json["thread_id"], "t1");
    }

    // ── Architecture tests ──

    #[test]
    fn test_event_id_equals_submission_id() {
        // Fundamental invariant: Event.id always correlates to Submission.id.
        let sub_id = SubmissionId("sub-corr-1".into());
        let event = Event {
            id: sub_id.clone(),
            msg: EventMsg::OpCompleted {
                thread_id: ThreadId::from_string("t"),
                turn_id: None,
                status: "ok".into(),
                user_input: String::new(),
            },
        };
        assert_eq!(event.id, sub_id);
    }

    #[test]
    fn test_all_event_msg_variants_roundtrip() {
        let variants: Vec<EventMsg> = vec![
            EventMsg::SubmissionStarted {
                thread_id: ThreadId::from_string("t"),
            },
            EventMsg::OpStarted {
                thread_id: ThreadId::from_string("t"),
            },
            EventMsg::OpCompleted {
                thread_id: ThreadId::from_string("t"),
                turn_id: Some(TurnId::from_string("turn-1")),
                status: "completed".into(),
                user_input: "hello".into(),
            },
            EventMsg::OpFailed {
                thread_id: ThreadId::from_string("t"),
                error: "fail".into(),
            },
            EventMsg::SubmissionCompleted {
                thread_id: ThreadId::from_string("t"),
            },
            EventMsg::Frame {
                seq: 1,
                frame: EventFrame::TurnStarted {
                    thread_id: ThreadId::from_string("t"),
                    turn_id: TurnId::from_string("turn-1"),
                    user_input: "hi".into(),
                },
            },
            EventMsg::StreamClosed {
                thread_id: ThreadId::from_string("t"),
            },
        ];

        for msg in variants {
            let json = serde_json::to_string(&msg).unwrap();
            let rt: EventMsg = serde_json::from_str(&json)
                .unwrap_or_else(|_| panic!("roundtrip failed for variant, json={json}"));
            // verify serialized form roundtrips to the same JSON
            let json2 = serde_json::to_string(&rt).unwrap();
            assert_eq!(json, json2, "roundtrip mismatch");
        }
    }
}
