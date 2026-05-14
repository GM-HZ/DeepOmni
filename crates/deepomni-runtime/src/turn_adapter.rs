use std::sync::Arc;

use deepomni_engine::{TurnOpError, TurnOpResult, TurnServices};
use deepomni_journal::JournalEntry;
use deepomni_protocol::event::{Event, EventMsg};
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::op::SubmissionId;
use deepomni_protocol::{CreateTurnRequest, Turn, TurnStatus};

use super::{Runtime, RuntimeError, RuntimeInner};

/// Thin adapter implementing TurnServices so the engine's TurnCoordinator can
/// dispatch ops to Runtime's low-level service methods.
///
/// The adapter returns Result<TurnOpResult, TurnOpError> — the coordinator
/// writes the outcome to the session event queue. There is no side-channel
/// (oneshot, reply map) for result delivery.
pub(super) struct RuntimeTurnAdapter {
    pub(super) inner: Arc<RuntimeInner>,
}

impl RuntimeTurnAdapter {
    fn runtime(&self) -> Runtime {
        Runtime::from_inner(self.inner.clone())
    }

    /// Send an event on the session event queue (Codex-style).
    fn send_event(&self, sub_id: &SubmissionId, msg: EventMsg) {
        let _ = self.inner.tx_event.send(Event {
            id: sub_id.clone(),
            msg,
        });
    }

    fn turn_status_to_op_status(status: &TurnStatus) -> String {
        match status {
            TurnStatus::Started => "started".into(),
            TurnStatus::Streaming => "streaming".into(),
            TurnStatus::WaitingForApproval => "waiting_for_approval".into(),
            TurnStatus::ExecutingTool => "executing_tool".into(),
            TurnStatus::Completed => "completed".into(),
            TurnStatus::Failed => "failed".into(),
            TurnStatus::Interrupted => "interrupted".into(),
        }
    }

    fn turn_result_to_op_result(
        thread_id: &ThreadId,
        result: Result<Turn, RuntimeError>,
    ) -> Result<TurnOpResult, TurnOpError> {
        match result {
            Ok(turn) => Ok(TurnOpResult {
                thread_id: thread_id.clone(),
                turn_id: Some(turn.id),
                status: Self::turn_status_to_op_status(&turn.status),
                user_input: turn.user_input,
            }),
            Err(e) => Err(TurnOpError::new(thread_id.clone(), format!("error: {e}"))),
        }
    }
}

#[async_trait::async_trait]
impl TurnServices for RuntimeTurnAdapter {
    async fn handle_user_input(
        &self,
        thread_id: ThreadId,
        text: String,
        model: Option<String>,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError> {
        // Codex-style: emit SubmissionStarted event.
        self.send_event(
            &submission_id,
            EventMsg::SubmissionStarted {
                thread_id: thread_id.clone(),
            },
        );

        let rt = self.runtime();
        let result = rt
            .submit_turn_direct(
                thread_id.clone(),
                CreateTurnRequest {
                    input: text,
                    model,
                    parent_turn_id: None,
                    subagent_id: None,
                    max_token_budget: None,
                },
            )
            .await;
        let op_result = Self::turn_result_to_op_result(&thread_id, result);
        match &op_result {
            Ok(r) if r.status != "waiting_for_approval" => {
                self.send_event(
                    &submission_id,
                    EventMsg::SubmissionCompleted {
                        thread_id: thread_id.clone(),
                    },
                );
            }
            Err(e) => {
                self.send_event(
                    &submission_id,
                    EventMsg::OpFailed {
                        thread_id: thread_id.clone(),
                        error: e.error.clone(),
                    },
                );
            }
            _ => {}
        }
        op_result
    }

    async fn handle_approval(
        &self,
        thread_id: ThreadId,
        approval_id: String,
        approved: bool,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError> {
        let rt = self.runtime();
        let pending = rt
            .inner
            .approval_coordinator
            .resolve_by_approval_id(&approval_id)
            .await;
        let result = match pending {
            Some(p) => {
                if approved {
                    rt.approve_tool_direct(thread_id.clone(), p.turn_id).await
                } else {
                    rt.reject_tool_direct(thread_id.clone(), p.turn_id).await
                }
            }
            None => Err(RuntimeError::NotReady(format!(
                "no pending approval for {approval_id}"
            ))),
        };
        let op_result = Self::turn_result_to_op_result(&thread_id, result);
        match &op_result {
            Ok(_) => {
                self.send_event(
                    &submission_id,
                    EventMsg::SubmissionCompleted {
                        thread_id: thread_id.clone(),
                    },
                );
            }
            Err(e) => {
                self.send_event(
                    &submission_id,
                    EventMsg::OpFailed {
                        thread_id: thread_id.clone(),
                        error: e.error.clone(),
                    },
                );
            }
        }
        op_result
    }

    async fn handle_cancel(
        &self,
        thread_id: ThreadId,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError> {
        let rt = self.runtime();
        let active_threads = rt.inner.active_threads.read().await;
        if let Some(active) = active_threads.get(&thread_id)
            && let Some(ref turn_id) = active.active_turn_id
        {
            let _ = rt.append_turn_journal(
                &thread_id,
                turn_id,
                JournalEntry::TurnInterrupted {
                    reason: "cancelled by user".into(),
                },
            );
        }
        self.send_event(
            &submission_id,
            EventMsg::SubmissionCompleted {
                thread_id: thread_id.clone(),
            },
        );
        Ok(TurnOpResult::ok(thread_id, None, "interrupted"))
    }

    async fn handle_compact(
        &self,
        thread_id: ThreadId,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError> {
        let rt = self.runtime();
        let mut threads = rt.inner.active_threads.write().await;
        match threads.get_mut(&thread_id) {
            Some(active) => {
                let mut ctx = active.context_manager.clone();
                let before = ctx.len();
                let removed = ctx.drop_last_n_user_turns(5);
                if removed == 0 && before > 20 {
                    ctx.replace_history(
                        ctx.for_prompt().into_iter().rev().take(20).rev().collect(),
                    );
                }
                let after = ctx.len();
                active.context_manager = ctx;
                let _ = rt.append_turn_journal(
                    &thread_id,
                    &TurnId::new(),
                    JournalEntry::ContextCompactionStarted,
                );
                let _ = rt.append_turn_journal(
                    &thread_id,
                    &TurnId::new(),
                    JournalEntry::ContextCompactionCompleted,
                );
                rt.inner.services.trace_writer.record_compaction(
                    &thread_id,
                    before as u64,
                    after as u64,
                );
                active
                    .compaction_tracker
                    .record_success(before as u64, after as u64);
                self.send_event(
                    &submission_id,
                    EventMsg::SubmissionCompleted {
                        thread_id: thread_id.clone(),
                    },
                );
                Ok(TurnOpResult::ok(thread_id, None, "completed"))
            }
            None => Err(TurnOpError::new(thread_id, "thread not found")),
        }
    }

    async fn handle_steer(
        &self,
        thread_id: ThreadId,
        steer_text: String,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError> {
        let rt = self.runtime();
        let active_threads = rt.inner.active_threads.read().await;
        if let Some(active) = active_threads.get(&thread_id)
            && let Some(ref turn_id) = active.active_turn_id
        {
            let _ = rt.append_turn_journal(
                &thread_id,
                turn_id,
                JournalEntry::AssistantDelta {
                    message_id: deepomni_protocol::id::MessageId::from_string("steer"),
                    delta: format!("<steer>{steer_text}</steer>"),
                },
            );
            self.send_event(
                &submission_id,
                EventMsg::SubmissionCompleted {
                    thread_id: thread_id.clone(),
                },
            );
            Ok(TurnOpResult::ok(
                thread_id,
                Some(turn_id.clone()),
                "started",
            ))
        } else {
            let tid = thread_id.clone();
            let err = TurnOpError::new(thread_id, "no active turn for steer input");
            self.send_event(
                &submission_id,
                EventMsg::OpFailed {
                    thread_id: tid,
                    error: err.error.clone(),
                },
            );
            Err(err)
        }
    }
}
