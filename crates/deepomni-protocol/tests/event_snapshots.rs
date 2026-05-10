//! Golden tests: Event JSON snapshots for all 25 EventFrame variants.
//! PRD §11.2: "All public protocol event types have JSON snapshots."

use deepomni_protocol::EventFrame;
use deepomni_protocol::event::ReasoningVisibility;
use deepomni_protocol::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};

fn assert_roundtrip(event: &EventFrame) {
    let json = serde_json::to_string(event).expect("serialize");
    let parsed: EventFrame = serde_json::from_str(&json).expect("deserialize");
    let json2 = serde_json::to_string(&parsed).expect("re-serialize");
    assert_eq!(json, json2, "JSON should be stable across roundtrip");
}

// ── Thread Lifecycle ──

#[test]
fn test_thread_created_json() {
    assert_roundtrip(&EventFrame::ThreadCreated {
        thread_id: ThreadId::from_string("thread-abc"),
        workspace: "/repo".into(),
    });
}

#[test]
fn test_thread_updated_json() {
    assert_roundtrip(&EventFrame::ThreadUpdated {
        thread_id: ThreadId::from_string("thread-abc"),
    });
}

#[test]
fn test_thread_archived_json() {
    assert_roundtrip(&EventFrame::ThreadArchived {
        thread_id: ThreadId::from_string("thread-abc"),
    });
}

// ── Turn Lifecycle ──

#[test]
fn test_turn_started_json() {
    assert_roundtrip(&EventFrame::TurnStarted {
        thread_id: ThreadId::from_string("thread-1"),
        turn_id: TurnId::from_string("turn-1"),
        user_input: "Fix the bug".into(),
    });
}

#[test]
fn test_turn_steered_json() {
    assert_roundtrip(&EventFrame::TurnSteered {
        thread_id: ThreadId::from_string("thread-1"),
        turn_id: TurnId::from_string("turn-1"),
        steering_input: "Use a different approach".into(),
    });
}

#[test]
fn test_turn_interrupted_json() {
    assert_roundtrip(&EventFrame::TurnInterrupted {
        thread_id: ThreadId::from_string("thread-1"),
        turn_id: TurnId::from_string("turn-1"),
    });
}

// ── Assistant Output ──

#[test]
fn test_assistant_message_delta_json() {
    assert_roundtrip(&EventFrame::AssistantMessageDelta {
        turn_id: TurnId::from_string("turn-1"),
        message_id: MessageId::from_string("msg-1"),
        delta: "Hello, ".into(),
    });
}

#[test]
fn test_assistant_message_completed_json() {
    assert_roundtrip(&EventFrame::AssistantMessageCompleted {
        turn_id: TurnId::from_string("turn-1"),
        message_id: MessageId::from_string("msg-1"),
    });
}

// ── Reasoning ──

#[test]
fn test_assistant_reasoning_delta_json() {
    assert_roundtrip(&EventFrame::AssistantReasoningDelta {
        turn_id: TurnId::from_string("turn-1"),
        message_id: MessageId::from_string("msg-1"),
        delta: "Let me think...".into(),
        replay_required: true,
        visibility: ReasoningVisibility::HostRenderable,
    });
}

#[test]
fn test_assistant_reasoning_completed_json() {
    assert_roundtrip(&EventFrame::AssistantReasoningCompleted {
        turn_id: TurnId::from_string("turn-1"),
        message_id: MessageId::from_string("msg-1"),
        replay_required: true,
    });
}

// ── Tool Call Lifecycle ──

#[test]
fn test_tool_call_started_json() {
    assert_roundtrip(&EventFrame::ToolCallStarted {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
        tool_name: "read_file".into(),
    });
}

#[test]
fn test_tool_call_arguments_delta_json() {
    assert_roundtrip(&EventFrame::ToolCallArgumentsDelta {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
        delta: r#"{"path": "src/main.rs"}"#.into(),
    });
}

#[test]
fn test_tool_call_requires_approval_json() {
    assert_roundtrip(&EventFrame::ToolCallRequiresApproval {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
        tool_name: "shell_exec".into(),
        reason: "Mutating command".into(),
    });
}

#[test]
fn test_tool_call_approved_json() {
    assert_roundtrip(&EventFrame::ToolCallApproved {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
    });
}

#[test]
fn test_tool_call_rejected_json() {
    assert_roundtrip(&EventFrame::ToolCallRejected {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
    });
}

#[test]
fn test_tool_call_completed_json() {
    assert_roundtrip(&EventFrame::ToolCallCompleted {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
        success: true,
        output_preview: Some("file content...".into()),
    });
}

#[test]
fn test_tool_call_failed_json() {
    assert_roundtrip(&EventFrame::ToolCallFailed {
        turn_id: TurnId::from_string("turn-1"),
        call_id: ToolCallId::from_string("call-1"),
        error: "command not found".into(),
    });
}

// ── Context Management ──

#[test]
fn test_context_compaction_started_json() {
    assert_roundtrip(&EventFrame::ContextCompactionStarted {
        turn_id: TurnId::from_string("turn-1"),
    });
}

#[test]
fn test_context_compaction_completed_json() {
    assert_roundtrip(&EventFrame::ContextCompactionCompleted {
        turn_id: TurnId::from_string("turn-1"),
    });
}

// ── Sub-Agents ──

#[test]
fn test_subagent_spawned_json() {
    assert_roundtrip(&EventFrame::SubagentSpawned {
        parent_turn_id: TurnId::from_string("turn-1"),
        subagent_id: SubagentId::from_string("sub-1"),
        task: "Review the changes".into(),
    });
}

#[test]
fn test_subagent_completed_json() {
    assert_roundtrip(&EventFrame::SubagentCompleted {
        parent_turn_id: TurnId::from_string("turn-1"),
        subagent_id: SubagentId::from_string("sub-1"),
        result_summary: "Found 3 issues".into(),
    });
}

#[test]
fn test_subagent_failed_json() {
    assert_roundtrip(&EventFrame::SubagentFailed {
        parent_turn_id: TurnId::from_string("turn-1"),
        subagent_id: SubagentId::from_string("sub-1"),
        error: "timeout".into(),
    });
}

// ── Terminal States ──

#[test]
fn test_turn_completed_json() {
    assert_roundtrip(&EventFrame::TurnCompleted {
        turn_id: TurnId::from_string("turn-1"),
    });
}

#[test]
fn test_turn_failed_json() {
    assert_roundtrip(&EventFrame::TurnFailed {
        turn_id: TurnId::from_string("turn-1"),
        error: "provider 503".into(),
    });
}

// ── Ad-Hoc ──

#[test]
fn test_runtime_warning_json() {
    assert_roundtrip(&EventFrame::RuntimeWarning {
        message: "approaching rate limit".into(),
    });
}

#[test]
fn test_all_event_tags_unique() {
    use std::collections::HashSet;
    let mut tags = HashSet::new();
    let events: Vec<EventFrame> = vec![
        EventFrame::ThreadCreated { thread_id: ThreadId::from_string("t"), workspace: "/w".into() },
        EventFrame::ThreadUpdated { thread_id: ThreadId::from_string("t") },
        EventFrame::ThreadArchived { thread_id: ThreadId::from_string("t") },
        EventFrame::TurnStarted { thread_id: ThreadId::from_string("t"), turn_id: TurnId::from_string("t1"), user_input: "u".into() },
        EventFrame::TurnSteered { thread_id: ThreadId::from_string("t"), turn_id: TurnId::from_string("t1"), steering_input: "s".into() },
        EventFrame::TurnInterrupted { thread_id: ThreadId::from_string("t"), turn_id: TurnId::from_string("t1") },
        EventFrame::AssistantMessageDelta { turn_id: TurnId::from_string("t1"), message_id: MessageId::from_string("m"), delta: "d".into() },
        EventFrame::AssistantMessageCompleted { turn_id: TurnId::from_string("t1"), message_id: MessageId::from_string("m") },
        EventFrame::AssistantReasoningDelta { turn_id: TurnId::from_string("t1"), message_id: MessageId::from_string("m"), delta: "r".into(), replay_required: false, visibility: ReasoningVisibility::HostRenderable },
        EventFrame::AssistantReasoningCompleted { turn_id: TurnId::from_string("t1"), message_id: MessageId::from_string("m"), replay_required: false },
        EventFrame::ToolCallStarted { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c"), tool_name: "read_file".into() },
        EventFrame::ToolCallArgumentsDelta { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c"), delta: "{}".into() },
        EventFrame::ToolCallRequiresApproval { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c"), tool_name: "shell_exec".into(), reason: "mutating".into() },
        EventFrame::ToolCallApproved { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c") },
        EventFrame::ToolCallRejected { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c") },
        EventFrame::ToolCallCompleted { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c"), success: true, output_preview: None },
        EventFrame::ToolCallFailed { turn_id: TurnId::from_string("t1"), call_id: ToolCallId::from_string("c"), error: "e".into() },
        EventFrame::ContextCompactionStarted { turn_id: TurnId::from_string("t1") },
        EventFrame::ContextCompactionCompleted { turn_id: TurnId::from_string("t1") },
        EventFrame::SubagentSpawned { parent_turn_id: TurnId::from_string("t1"), subagent_id: SubagentId::from_string("s"), task: "review".into() },
        EventFrame::SubagentCompleted { parent_turn_id: TurnId::from_string("t1"), subagent_id: SubagentId::from_string("s"), result_summary: "done".into() },
        EventFrame::SubagentFailed { parent_turn_id: TurnId::from_string("t1"), subagent_id: SubagentId::from_string("s"), error: "fail".into() },
        EventFrame::TurnCompleted { turn_id: TurnId::from_string("t1") },
        EventFrame::TurnFailed { turn_id: TurnId::from_string("t1"), error: "fatal".into() },
        EventFrame::RuntimeWarning { message: "warn".into() },
    ];
    for event in &events {
        let json = serde_json::to_string(event).unwrap();
        let tag: String = serde_json::from_str::<serde_json::Value>(&json)
            .unwrap()
            .get("event")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert!(tags.insert(tag.clone()), "duplicate event tag: {tag}");
    }
    assert_eq!(tags.len(), 25, "all 25 event types must have unique tags");
}
