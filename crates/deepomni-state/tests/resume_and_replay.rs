//! Integration test: Thread resume and event replay from persisted state.
//! PRD §11.2: "Resume thread from state" and "SSE replay".

use deepomni_protocol::EventFrame;
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_state::{StateStore, ThreadRecord, TurnRecord};
use deepomni_journal::{JournalEntry, TurnJournal};

fn test_store() -> StateStore {
    let path = std::env::temp_dir().join(format!("state-integration-{}.db", uuid::Uuid::new_v4()));
    StateStore::open(Some(path)).unwrap()
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[test]
fn test_persist_and_resume_thread() {
    let store = test_store();
    let thread_id = "thread-resume-1";

    // Create thread.
    let thread = ThreadRecord {
        id: thread_id.to_string(),
        preview: "test thread".into(),
        ephemeral: false,
        model_provider: "deepseek".into(),
        created_at: current_timestamp(),
        updated_at: current_timestamp(),
        status: "idle".into(),
        path: None,
        cwd: "/workspace".into(),
        cli_version: "0.1.0".into(),
        source: "interactive".into(),
        name: Some("Resume Test".into()),
        sandbox_policy: None,
        approval_mode: None,
        archived: false,
        archived_at: None,
        parent_thread_id: None,
    };
    store.upsert_thread(&thread).unwrap();

    // Read back.
    let restored = store.get_thread(thread_id).unwrap().unwrap();
    assert_eq!(restored.id, thread_id);
    assert_eq!(restored.name.unwrap(), "Resume Test");
    assert_eq!(restored.cwd, "/workspace");
}

#[test]
fn test_event_replay_since_seq() {
    let store = test_store();
    let thread_id = "thread-replay-1";
    let turn_id = "turn-replay-1";

    // Create parent records.
    store.upsert_thread(&ThreadRecord {
        id: thread_id.to_string(),
        preview: "".into(),
        ephemeral: false,
        model_provider: "deepseek".into(),
        created_at: current_timestamp(),
        updated_at: current_timestamp(),
        status: "idle".into(),
        path: None,
        cwd: ".".into(),
        cli_version: "0.1.0".into(),
        source: "interactive".into(),
        name: None,
        sandbox_policy: None,
        approval_mode: None,
        archived: false,
        archived_at: None,
        parent_thread_id: None,
    }).unwrap();
    store.insert_turn(&TurnRecord {
        id: turn_id.to_string(),
        thread_id: thread_id.to_string(),
        status: "started".into(),
        user_input: "hello".into(),
        created_at: current_timestamp(),
        completed_at: None,
        model: None,
        model_provider: None,
        parent_turn_id: None,
        parent_thread_id: None,
        subagent_id: None,
    }).unwrap();

    // Insert events with sequential seq numbers.
    let events: Vec<(i64, EventFrame)> = vec![
        (1, EventFrame::TurnStarted {
            thread_id: ThreadId::from_string(thread_id),
            turn_id: TurnId::from_string(turn_id),
            user_input: "hello".into(),
        }),
        (2, EventFrame::AssistantMessageDelta {
            turn_id: TurnId::from_string(turn_id),
            message_id: deepomni_protocol::id::MessageId::from_string("msg-1"),
            delta: "Hi".into(),
        }),
        (3, EventFrame::AssistantMessageCompleted {
            turn_id: TurnId::from_string(turn_id),
            message_id: deepomni_protocol::id::MessageId::from_string("msg-1"),
        }),
        (4, EventFrame::TurnCompleted {
            turn_id: TurnId::from_string(turn_id),
        }),
    ];

    for (seq, event) in &events {
        store.insert_event(thread_id, turn_id, *seq, event).unwrap();
    }

    // Replay from seq 0 (all events).
    let all = store.get_events_since(thread_id, 0).unwrap();
    assert_eq!(all.len(), 4);

    // Replay from seq 2 (only events after seq 2).
    let later = store.get_events_since(thread_id, 2).unwrap();
    assert_eq!(later.len(), 2);
    assert_eq!(later[0].0, 3);
    assert_eq!(later[1].0, 4);
}

#[test]
fn test_journal_append_replay_and_pending_approval_projection() {
    let store = test_store();
    let thread_id = ThreadId::from_string("thread-journal-1");
    let turn_id = TurnId::from_string("turn-journal-1");

    store.upsert_thread(&ThreadRecord {
        id: thread_id.to_string(),
        preview: "".into(),
        ephemeral: false,
        model_provider: "deepseek".into(),
        created_at: current_timestamp(),
        updated_at: current_timestamp(),
        status: "idle".into(),
        path: None,
        cwd: ".".into(),
        cli_version: "0.1.0".into(),
        source: "interactive".into(),
        name: None,
        sandbox_policy: None,
        approval_mode: None,
        archived: false,
        archived_at: None,
        parent_thread_id: None,
    }).unwrap();

    let first = store
        .append(
            &thread_id,
            &turn_id,
            JournalEntry::TurnStarted {
                user_input: "hello".into(),
            },
        )
        .unwrap();
    let second = store
        .append(
            &thread_id,
            &turn_id,
            JournalEntry::ApprovalPending {
                call_id: deepomni_protocol::ToolCallId::from_string("call-1"),
                approval_id: "approval-1".into(),
                tool_name: "write_file".into(),
                arguments: serde_json::json!({"path": "README.md"}),
                reason: "mutating tool".into(),
                model: "deepseek-chat".into(),
                workspace: "/workspace".into(),
                config_json: "{\"model\":\"deepseek-chat\"}".into(),
            },
        )
        .unwrap();

    assert_eq!(first.seq, 1);
    assert_eq!(second.seq, 2);

    let replayed = store.replay(&thread_id, 0).unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].seq, 1);
    assert_eq!(replayed[1].seq, 2);

    let pending = store
        .get_pending_approval_by_turn(thread_id.as_str(), turn_id.as_str())
        .unwrap()
        .unwrap();
    assert_eq!(pending.approval_id, "approval-1");
    assert_eq!(pending.tool_name, "write_file");
    assert_eq!(pending.arguments_json, "{\"path\":\"README.md\"}");

    store
        .append(
            &thread_id,
            &turn_id,
            JournalEntry::ApprovalResolved {
                approval_id: "approval-1".into(),
                approved: true,
            },
        )
        .unwrap();

    assert!(
        store
            .get_pending_approval_by_turn(thread_id.as_str(), turn_id.as_str())
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_thread_list() {
    let store = test_store();

    for i in 1..=3 {
        store.upsert_thread(&ThreadRecord {
            id: format!("thread-list-{i}"),
            preview: format!("thread {i}"),
            ephemeral: false,
            model_provider: "deepseek".into(),
            created_at: current_timestamp(),
            updated_at: current_timestamp(),
            status: if i == 3 { "archived".into() } else { "idle".into() },
            path: None,
            cwd: ".".into(),
            cli_version: "0.1.0".into(),
            source: "interactive".into(),
            name: None,
            sandbox_policy: None,
            approval_mode: None,
            archived: i == 3,
            archived_at: None,
            parent_thread_id: None,
        }).unwrap();
    }

    // Without archived.
    let active = store.list_threads(false, 100).unwrap();
    assert_eq!(active.len(), 2, "should not include archived threads");

    // With archived.
    let all = store.list_threads(true, 100).unwrap();
    assert_eq!(all.len(), 3, "should include archived threads");
}

#[test]
fn test_monotonic_seq_generation() {
    let store = test_store();

    // Create parent thread.
    store.upsert_thread(&ThreadRecord {
        id: "thread-seq".into(),
        preview: "".into(),
        ephemeral: false,
        model_provider: "deepseek".into(),
        created_at: current_timestamp(),
        updated_at: current_timestamp(),
        status: "idle".into(),
        path: None,
        cwd: ".".into(),
        cli_version: "0.1.0".into(),
        source: "interactive".into(),
        name: None,
        sandbox_policy: None,
        approval_mode: None,
        archived: false,
        archived_at: None,
        parent_thread_id: None,
    }).unwrap();

    // Next seq on a brand new thread should start at 1.
    assert_eq!(store.next_event_seq("thread-seq").unwrap(), 1);

    // After inserting event at seq 1, next should be 2.
    store.insert_event("thread-seq", "turn-seq", 1, &EventFrame::TurnStarted {
        thread_id: ThreadId::from_string("thread-seq"),
        turn_id: TurnId::from_string("turn-seq"),
        user_input: "test".into(),
    }).unwrap();
    assert_eq!(store.next_event_seq("thread-seq").unwrap(), 2);
}
