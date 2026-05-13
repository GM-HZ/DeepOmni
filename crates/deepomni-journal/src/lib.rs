//! Append-only turn journal primitives.
//!
//! `JournalEntry` is DeepOmni's internal turn record. Public hosts still see
//! `EventFrame`; journal records are projected into host-safe event frames.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

use deepomni_protocol::EventFrame;
use deepomni_protocol::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};

/// The internal source of truth for everything that happens in a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEntry {
    TurnStarted {
        user_input: String,
    },
    TurnCompleted,
    TurnFailed {
        error: String,
    },
    AssistantDelta {
        message_id: MessageId,
        delta: String,
    },
    AssistantCompleted {
        message_id: MessageId,
    },
    ReasoningBlock {
        message_id: MessageId,
        content: String,
        replay_required: bool,
    },
    ReasoningDelta {
        message_id: MessageId,
        delta: String,
        replay_required: bool,
    },
    ToolCallRequested {
        call_id: ToolCallId,
        tool_name: String,
        arguments: Value,
    },
    ToolCallArgumentsDelta {
        call_id: ToolCallId,
        delta: String,
    },
    ToolCallApproved {
        call_id: ToolCallId,
    },
    ToolCallRejected {
        call_id: ToolCallId,
    },
    ToolCallCompleted {
        call_id: ToolCallId,
        tool_name: String,
        success: bool,
        output: Option<String>,
        output_preview: Option<String>,
    },
    ToolCallFailed {
        call_id: ToolCallId,
        error: String,
    },
    ApprovalPending {
        call_id: ToolCallId,
        approval_id: String,
        tool_name: String,
        arguments: Value,
        reason: String,
        model: String,
        workspace: String,
        config_json: String,
    },
    ApprovalResolved {
        approval_id: String,
        approved: bool,
    },
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
    ContextCompactionStarted,
    ContextCompactionCompleted,
    TurnInterrupted {
        reason: String,
    },
    ProviderUsage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
}

/// Durable/replayable journal envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRecord {
    pub seq: i64,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub entry: JournalEntry,
    pub created_at: i64,
}

pub trait TurnJournal: Send + Sync {
    fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        entry: JournalEntry,
    ) -> Result<JournalRecord, JournalError>;

    fn replay(
        &self,
        thread_id: &ThreadId,
        since_seq: i64,
    ) -> Result<Vec<JournalRecord>, JournalError>;

    fn subscribe(&self, thread_id: ThreadId) -> JournalSubscriber;
}

#[derive(Debug)]
pub struct JournalSubscriber {
    pub thread_id: ThreadId,
    rx: broadcast::Receiver<JournalRecord>,
}

impl JournalSubscriber {
    pub async fn recv(&mut self) -> Result<JournalRecord, broadcast::error::RecvError> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Result<JournalRecord, broadcast::error::TryRecvError> {
        self.rx.try_recv()
    }
}

pub fn subscriber(
    thread_id: ThreadId,
    rx: broadcast::Receiver<JournalRecord>,
) -> JournalSubscriber {
    JournalSubscriber { thread_id, rx }
}

pub fn journal_to_event_frame(record: &JournalRecord) -> Option<EventFrame> {
    match &record.entry {
        JournalEntry::TurnStarted { user_input } => Some(EventFrame::TurnStarted {
            thread_id: record.thread_id.clone(),
            turn_id: record.turn_id.clone(),
            user_input: user_input.clone(),
        }),
        JournalEntry::TurnCompleted => Some(EventFrame::TurnCompleted {
            turn_id: record.turn_id.clone(),
        }),
        JournalEntry::TurnInterrupted { .. } => Some(EventFrame::TurnInterrupted {
            thread_id: record.thread_id.clone(),
            turn_id: record.turn_id.clone(),
        }),
        JournalEntry::TurnFailed { error } => Some(EventFrame::TurnFailed {
            turn_id: record.turn_id.clone(),
            error: error.clone(),
        }),
        JournalEntry::AssistantDelta { message_id, delta } => {
            Some(EventFrame::AssistantMessageDelta {
                turn_id: record.turn_id.clone(),
                message_id: message_id.clone(),
                delta: delta.clone(),
            })
        }
        JournalEntry::AssistantCompleted { message_id } => {
            Some(EventFrame::AssistantMessageCompleted {
                turn_id: record.turn_id.clone(),
                message_id: message_id.clone(),
            })
        }
        JournalEntry::ReasoningBlock {
            message_id,
            replay_required,
            ..
        } => Some(EventFrame::AssistantReasoningCompleted {
            turn_id: record.turn_id.clone(),
            message_id: message_id.clone(),
            replay_required: *replay_required,
        }),
        JournalEntry::ReasoningDelta {
            message_id,
            delta,
            replay_required,
        } => Some(EventFrame::AssistantReasoningDelta {
            turn_id: record.turn_id.clone(),
            message_id: message_id.clone(),
            delta: delta.clone(),
            replay_required: *replay_required,
            visibility: deepomni_protocol::event::ReasoningVisibility::HostRenderable,
        }),
        JournalEntry::ToolCallRequested {
            call_id, tool_name, ..
        } => Some(EventFrame::ToolCallStarted {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
        }),
        JournalEntry::ToolCallArgumentsDelta { call_id, delta } => {
            Some(EventFrame::ToolCallArgumentsDelta {
                turn_id: record.turn_id.clone(),
                call_id: call_id.clone(),
                delta: delta.clone(),
            })
        }
        JournalEntry::ToolCallApproved { call_id } => Some(EventFrame::ToolCallApproved {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
        }),
        JournalEntry::ToolCallRejected { call_id } => Some(EventFrame::ToolCallRejected {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
        }),
        JournalEntry::ToolCallCompleted {
            call_id,
            tool_name,
            success,
            output_preview,
            ..
        } => Some(EventFrame::ToolCallCompleted {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            success: *success,
            output_preview: output_preview.clone(),
        }),
        JournalEntry::ToolCallFailed { call_id, error } => Some(EventFrame::ToolCallFailed {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
            error: error.clone(),
        }),
        JournalEntry::ApprovalPending {
            call_id,
            tool_name,
            reason,
            ..
        } => Some(EventFrame::ToolCallRequiresApproval {
            turn_id: record.turn_id.clone(),
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            reason: reason.clone(),
        }),
        JournalEntry::ApprovalResolved { .. } => None,
        JournalEntry::SubagentSpawned {
            parent_turn_id,
            subagent_id,
            task,
        } => Some(EventFrame::SubagentSpawned {
            parent_turn_id: parent_turn_id.clone(),
            subagent_id: subagent_id.clone(),
            task: task.clone(),
        }),
        JournalEntry::SubagentCompleted {
            parent_turn_id,
            subagent_id,
            result_summary,
        } => Some(EventFrame::SubagentCompleted {
            parent_turn_id: parent_turn_id.clone(),
            subagent_id: subagent_id.clone(),
            result_summary: result_summary.clone(),
        }),
        JournalEntry::SubagentFailed {
            parent_turn_id,
            subagent_id,
            error,
        } => Some(EventFrame::SubagentFailed {
            parent_turn_id: parent_turn_id.clone(),
            subagent_id: subagent_id.clone(),
            error: error.clone(),
        }),
        JournalEntry::ContextCompactionStarted => Some(EventFrame::ContextCompactionStarted {
            turn_id: record.turn_id.clone(),
        }),
        JournalEntry::ContextCompactionCompleted => Some(EventFrame::ContextCompactionCompleted {
            turn_id: record.turn_id.clone(),
        }),
        JournalEntry::ProviderUsage { .. } => None,
    }
}

#[derive(Debug, Default)]
pub struct InMemoryJournal {
    inner: Mutex<InMemoryJournalInner>,
}

#[derive(Debug, Default)]
struct InMemoryJournalInner {
    records: HashMap<String, Vec<JournalRecord>>,
    senders: HashMap<String, broadcast::Sender<JournalRecord>>,
}

impl InMemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    fn current_timestamp() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

impl TurnJournal for InMemoryJournal {
    fn append(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        entry: JournalEntry,
    ) -> Result<JournalRecord, JournalError> {
        let mut inner = self.inner.lock().expect("in-memory journal mutex poisoned");
        let records = inner.records.entry(thread_id.to_string()).or_default();
        let record = JournalRecord {
            seq: records.last().map(|record| record.seq + 1).unwrap_or(1),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            entry,
            created_at: Self::current_timestamp(),
        };
        records.push(record.clone());
        if let Some(sender) = inner.senders.get(thread_id.as_str()) {
            let _ = sender.send(record.clone());
        }
        Ok(record)
    }

    fn replay(
        &self,
        thread_id: &ThreadId,
        since_seq: i64,
    ) -> Result<Vec<JournalRecord>, JournalError> {
        let inner = self.inner.lock().expect("in-memory journal mutex poisoned");
        Ok(inner
            .records
            .get(thread_id.as_str())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.seq > since_seq)
            .collect())
    }

    fn subscribe(&self, thread_id: ThreadId) -> JournalSubscriber {
        let sender = {
            let mut inner = self.inner.lock().expect("in-memory journal mutex poisoned");
            inner
                .senders
                .entry(thread_id.to_string())
                .or_insert_with(|| broadcast::channel(256).0)
                .clone()
        };
        subscriber(thread_id, sender.subscribe())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("journal storage error: {0}")]
    Storage(String),
    #[error("journal serialization error: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::id::{ThreadId, TurnId};

    #[test]
    fn journal_record_serializes_with_envelope_and_entry_payload() {
        let record = JournalRecord {
            seq: 7,
            thread_id: ThreadId::from_string("thread-1"),
            turn_id: TurnId::from_string("turn-1"),
            entry: JournalEntry::TurnStarted {
                user_input: "hello".to_string(),
            },
            created_at: 1_700_000_000,
        };

        let json = serde_json::to_value(&record).unwrap();

        assert_eq!(json["seq"], 7);
        assert_eq!(json["thread_id"], "thread-1");
        assert_eq!(json["turn_id"], "turn-1");
        assert_eq!(json["created_at"], 1_700_000_000);
        assert_eq!(json["entry"]["type"], "turn_started");
        assert_eq!(json["entry"]["user_input"], "hello");
    }

    #[test]
    fn projects_turn_started_record_to_public_event_frame() {
        let record = JournalRecord {
            seq: 1,
            thread_id: ThreadId::from_string("thread-1"),
            turn_id: TurnId::from_string("turn-1"),
            entry: JournalEntry::TurnStarted {
                user_input: "hello".to_string(),
            },
            created_at: 1_700_000_000,
        };

        let event = journal_to_event_frame(&record).unwrap();

        match event {
            deepomni_protocol::EventFrame::TurnStarted {
                thread_id,
                turn_id,
                user_input,
            } => {
                assert_eq!(thread_id.as_str(), "thread-1");
                assert_eq!(turn_id.as_str(), "turn-1");
                assert_eq!(user_input, "hello");
            }
            other => panic!("unexpected projection: {other:?}"),
        }
    }
}
