//! TurnCoordinator — implements OpHandler and dispatches ops through
//! a TurnServices trait. Phase C: Moves op dispatch from Runtime to Engine.

use std::sync::Arc;

use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::op::{Op, Submission, SubmissionId};

use crate::OpHandler;

/// Services the TurnCoordinator needs from the runtime for turn execution.
/// Implemented by the runtime as a thin adapter over the existing methods.
/// Reply delivery is handled internally by the adapter.
#[async_trait::async_trait]
pub trait TurnServices: Send + Sync + 'static {
    /// Handle a UserInput op — run a new turn on the given thread.
    async fn handle_user_input(
        &self,
        thread_id: ThreadId,
        text_input: String,
        model: Option<String>,
        submission_id: SubmissionId,
    );

    /// Handle an ApprovalDecision op.
    async fn handle_approval(
        &self,
        thread_id: ThreadId,
        approval_id: String,
        approved: bool,
        submission_id: SubmissionId,
    );

    /// Handle a Cancel op.
    async fn handle_cancel(&self, thread_id: ThreadId, submission_id: SubmissionId);

    /// Handle a Compact op.
    async fn handle_compact(&self, thread_id: ThreadId, submission_id: SubmissionId);

    /// Handle a SteerInput op.
    async fn handle_steer(
        &self,
        thread_id: ThreadId,
        steer_text: String,
        submission_id: SubmissionId,
    );
}

/// Result of processing a turn op. Simple enough that engine doesn't
/// need to depend on agent or runtime types.
#[derive(Debug, Clone)]
pub struct TurnOpResult {
    pub thread_id: ThreadId,
    pub turn_id: Option<TurnId>,
    pub status: String,
    pub user_input: String,
}

impl TurnOpResult {
    pub fn ok(thread_id: ThreadId, turn_id: Option<TurnId>, status: &str) -> Self {
        Self {
            thread_id,
            turn_id,
            status: status.to_string(),
            user_input: String::new(),
        }
    }

    pub fn err(thread_id: ThreadId, status: String) -> Self {
        Self {
            thread_id,
            turn_id: None,
            status,
            user_input: String::new(),
        }
    }
}

/// TurnCoordinator implements OpHandler by dispatching through TurnServices.
/// Reply delivery is handled by the adapter (which has access to submission_replies).
pub struct TurnCoordinator {
    services: Arc<dyn TurnServices>,
}

impl TurnCoordinator {
    pub fn new(services: Arc<dyn TurnServices>) -> Self {
        Self { services }
    }
}

#[async_trait::async_trait]
impl OpHandler for TurnCoordinator {
    async fn handle_op(&self, submission: Submission) {
        match submission.op {
            Op::UserInput {
                thread_id,
                input,
                settings,
            } => {
                let text = input
                    .iter()
                    .find_map(|ui| match ui {
                        deepomni_protocol::op::UserInput::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                self.services
                    .handle_user_input(thread_id, text, settings.model, submission.id)
                    .await
            }
            Op::ApprovalDecision {
                thread_id,
                approval_id,
                approved,
            } => {
                self.services
                    .handle_approval(thread_id, approval_id, approved, submission.id)
                    .await
            }
            Op::Cancel { thread_id } => self.services.handle_cancel(thread_id, submission.id).await,
            Op::Compact { thread_id } => {
                self.services.handle_compact(thread_id, submission.id).await
            }
            Op::SteerInput { thread_id, input } => {
                let text = input
                    .iter()
                    .find_map(|ui| match ui {
                        deepomni_protocol::op::UserInput::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                self.services
                    .handle_steer(thread_id, text, submission.id)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    struct MockServices {
        tx: mpsc::UnboundedSender<String>,
    }

    #[async_trait::async_trait]
    impl TurnServices for MockServices {
        async fn handle_user_input(
            &self,
            _thread_id: ThreadId,
            text: String,
            _model: Option<String>,
            _submission_id: SubmissionId,
        ) {
            let _ = self.tx.send(format!("user_input:{text}"));
        }
        async fn handle_approval(
            &self,
            _thread_id: ThreadId,
            approval_id: String,
            approved: bool,
            _submission_id: SubmissionId,
        ) {
            let _ = self.tx.send(format!("approval:{approval_id}:{approved}"));
        }
        async fn handle_cancel(&self, _thread_id: ThreadId, _submission_id: SubmissionId) {
            let _ = self.tx.send("cancel".into());
        }
        async fn handle_compact(&self, _thread_id: ThreadId, _submission_id: SubmissionId) {
            let _ = self.tx.send("compact".into());
        }
        async fn handle_steer(
            &self,
            _thread_id: ThreadId,
            text: String,
            _submission_id: SubmissionId,
        ) {
            let _ = self.tx.send(format!("steer:{text}"));
        }
    }

    #[tokio::test]
    async fn test_turn_coordinator_dispatches_all_ops() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let services = Arc::new(MockServices { tx });
        let coordinator = TurnCoordinator::new(services);

        // Submit UserInput op.
        coordinator
            .handle_op(Submission {
                id: SubmissionId::new(),
                op: Op::UserInput {
                    thread_id: ThreadId::from_string("t"),
                    input: vec![deepomni_protocol::op::UserInput::Text {
                        text: "hello".into(),
                    }],
                    settings: Default::default(),
                },
            })
            .await;
        assert_eq!(rx.try_recv().unwrap(), "user_input:hello");

        // Submit Cancel op.
        coordinator
            .handle_op(Submission {
                id: SubmissionId::new(),
                op: Op::Cancel {
                    thread_id: ThreadId::from_string("t"),
                },
            })
            .await;
        assert_eq!(rx.try_recv().unwrap(), "cancel");

        // Submit Compact op.
        coordinator
            .handle_op(Submission {
                id: SubmissionId::new(),
                op: Op::Compact {
                    thread_id: ThreadId::from_string("t"),
                },
            })
            .await;
        assert_eq!(rx.try_recv().unwrap(), "compact");

        // Submit SteerInput op.
        coordinator
            .handle_op(Submission {
                id: SubmissionId::new(),
                op: Op::SteerInput {
                    thread_id: ThreadId::from_string("t"),
                    input: vec![deepomni_protocol::op::UserInput::Text {
                        text: "steer me".into(),
                    }],
                },
            })
            .await;
        assert_eq!(rx.try_recv().unwrap(), "steer:steer me");

        // Submit ApprovalDecision op.
        coordinator
            .handle_op(Submission {
                id: SubmissionId::new(),
                op: Op::ApprovalDecision {
                    thread_id: ThreadId::from_string("t"),
                    approval_id: "approval-1".into(),
                    approved: true,
                },
            })
            .await;
        assert_eq!(rx.try_recv().unwrap(), "approval:approval-1:true");
    }
}
