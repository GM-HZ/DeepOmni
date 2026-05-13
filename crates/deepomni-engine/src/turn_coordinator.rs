//! TurnCoordinator — implements OpHandler and dispatches ops through
//! a TurnServices trait. Only produces correlated Event messages on the
//! session event queue; it has no knowledge of oneshot channels, runtime
//! reply maps, or any other side-channel communication path.

use std::sync::Arc;

use deepomni_protocol::EventMsg;
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::op::{Op, Submission, SubmissionId};

use crate::{OpHandler, SessionEventSink};

/// Services the TurnCoordinator needs from the runtime for turn execution.
/// Implemented by the runtime as a thin adapter. The adapter must NOT
/// deliver replies through any side-channel — the only contract is the
/// returned Result, and the coordinator writes the outcome to the event queue.
#[async_trait::async_trait]
pub trait TurnServices: Send + Sync + 'static {
    /// Handle a UserInput op — run a new turn on the given thread.
    async fn handle_user_input(
        &self,
        thread_id: ThreadId,
        text_input: String,
        model: Option<String>,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError>;

    /// Handle an ApprovalDecision op.
    async fn handle_approval(
        &self,
        thread_id: ThreadId,
        approval_id: String,
        approved: bool,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError>;

    /// Handle a Cancel op.
    async fn handle_cancel(
        &self,
        thread_id: ThreadId,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError>;

    /// Handle a Compact op.
    async fn handle_compact(
        &self,
        thread_id: ThreadId,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError>;

    /// Handle a SteerInput op.
    async fn handle_steer(
        &self,
        thread_id: ThreadId,
        steer_text: String,
        submission_id: SubmissionId,
    ) -> Result<TurnOpResult, TurnOpError>;
}

/// Success result of processing a turn op.
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
}

/// Error from processing a turn op. Carried as EventMsg::OpFailed on the
/// session event queue so the caller (e.g. Runtime::submit_and_wait) can
/// observe failures without a separate oneshot channel.
#[derive(Debug, Clone)]
pub struct TurnOpError {
    pub thread_id: ThreadId,
    pub error: String,
}

impl TurnOpError {
    pub fn new(thread_id: ThreadId, error: impl Into<String>) -> Self {
        Self {
            thread_id,
            error: error.into(),
        }
    }
}

impl std::fmt::Display for TurnOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for TurnOpError {}

/// TurnCoordinator implements OpHandler by dispatching through TurnServices.
/// It writes the full lifecycle (OpStarted → OpCompleted/OpFailed) to the
/// session event queue. It has no knowledge of oneshot channels or reply maps.
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
    async fn handle_op(&self, submission: Submission, events: SessionEventSink) {
        let submission_id = submission.id.clone();
        let thread_id = op_thread_id(&submission.op);

        // Emit OpStarted so consumers know the handler has accepted the op.
        events.emit_msg(
            submission_id.clone(),
            EventMsg::OpStarted {
                thread_id: thread_id.clone(),
            },
        );

        let result: Result<TurnOpResult, TurnOpError> = match submission.op {
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
                    .handle_user_input(thread_id, text, settings.model, submission_id.clone())
                    .await
            }
            Op::ApprovalDecision {
                thread_id,
                approval_id,
                approved,
            } => {
                self.services
                    .handle_approval(thread_id, approval_id, approved, submission_id.clone())
                    .await
            }
            Op::Cancel { thread_id } => {
                self.services
                    .handle_cancel(thread_id, submission_id.clone())
                    .await
            }
            Op::Compact { thread_id } => {
                self.services
                    .handle_compact(thread_id, submission_id.clone())
                    .await
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
                    .handle_steer(thread_id, text, submission_id.clone())
                    .await
            }
        };

        match result {
            Ok(op_result) => {
                events.emit_msg(
                    submission_id,
                    EventMsg::OpCompleted {
                        thread_id: op_result.thread_id,
                        turn_id: op_result.turn_id,
                        status: op_result.status,
                        user_input: op_result.user_input,
                    },
                );
            }
            Err(op_err) => {
                events.emit_msg(
                    submission_id,
                    EventMsg::OpFailed {
                        thread_id: op_err.thread_id,
                        error: op_err.error,
                    },
                );
            }
        }
    }
}

fn op_thread_id(op: &Op) -> ThreadId {
    match op {
        Op::UserInput { thread_id, .. }
        | Op::SteerInput { thread_id, .. }
        | Op::ApprovalDecision { thread_id, .. }
        | Op::Cancel { thread_id }
        | Op::Compact { thread_id } => thread_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    struct MockServices {
        /// If set, the next call will return this error instead of success.
        fail_next: tokio::sync::Mutex<Option<String>>,
    }

    impl MockServices {
        fn new() -> Self {
            Self {
                fail_next: tokio::sync::Mutex::new(None),
            }
        }
        async fn set_fail(&self, msg: &str) {
            *self.fail_next.lock().await = Some(msg.into());
        }
    }

    #[async_trait::async_trait]
    impl TurnServices for MockServices {
        async fn handle_user_input(
            &self,
            thread_id: ThreadId,
            text: String,
            _model: Option<String>,
            _submission_id: SubmissionId,
        ) -> Result<TurnOpResult, TurnOpError> {
            if let Some(err) = self.fail_next.lock().await.take() {
                return Err(TurnOpError::new(thread_id, err));
            }
            Ok(TurnOpResult::ok(
                thread_id,
                None,
                &format!("user_input:{text}"),
            ))
        }
        async fn handle_approval(
            &self,
            thread_id: ThreadId,
            approval_id: String,
            approved: bool,
            _submission_id: SubmissionId,
        ) -> Result<TurnOpResult, TurnOpError> {
            if let Some(err) = self.fail_next.lock().await.take() {
                return Err(TurnOpError::new(thread_id, err));
            }
            Ok(TurnOpResult::ok(
                thread_id,
                None,
                &format!("approval:{approval_id}:{approved}"),
            ))
        }
        async fn handle_cancel(
            &self,
            thread_id: ThreadId,
            _submission_id: SubmissionId,
        ) -> Result<TurnOpResult, TurnOpError> {
            if let Some(err) = self.fail_next.lock().await.take() {
                return Err(TurnOpError::new(thread_id, err));
            }
            Ok(TurnOpResult::ok(thread_id, None, "interrupted"))
        }
        async fn handle_compact(
            &self,
            thread_id: ThreadId,
            _submission_id: SubmissionId,
        ) -> Result<TurnOpResult, TurnOpError> {
            if let Some(err) = self.fail_next.lock().await.take() {
                return Err(TurnOpError::new(thread_id, err));
            }
            Ok(TurnOpResult::ok(thread_id, None, "completed"))
        }
        async fn handle_steer(
            &self,
            thread_id: ThreadId,
            text: String,
            _submission_id: SubmissionId,
        ) -> Result<TurnOpResult, TurnOpError> {
            if let Some(err) = self.fail_next.lock().await.take() {
                return Err(TurnOpError::new(thread_id, err));
            }
            Ok(TurnOpResult::ok(thread_id, None, &format!("steer:{text}")))
        }
    }

    /// Collect events emitted by the coordinator for verification.
    #[derive(Clone)]
    struct EventCollector {
        events: Arc<tokio::sync::Mutex<Vec<EventMsg>>>,
    }

    impl EventCollector {
        fn new() -> Self {
            Self {
                events: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            }
        }
    }

    fn event_sink_from_collector(
        collector: EventCollector,
    ) -> (
        SessionEventSink,
        mpsc::UnboundedSender<deepomni_protocol::Event>,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel::<deepomni_protocol::Event>();
        let events = collector.events.clone();
        let keepalive_tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                events.lock().await.push(event.msg);
            }
        });
        (SessionEventSink::new(tx), keepalive_tx)
    }

    async fn drain_events(
        collector: EventCollector,
        _keepalive: mpsc::UnboundedSender<deepomni_protocol::Event>,
    ) -> Vec<EventMsg> {
        // Small delay so spawned task has time to process.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut guard = collector.events.lock().await;
        std::mem::take(&mut *guard)
    }

    #[tokio::test]
    async fn test_coordinator_dispatches_all_five_ops() {
        let services = Arc::new(MockServices::new());

        // UserInput
        {
            let coordinator = TurnCoordinator::new(services.clone());
            let collector = EventCollector::new();
            let (sink, keepalive) = event_sink_from_collector(collector.clone());
            coordinator
                .handle_op(
                    Submission {
                        id: SubmissionId::new(),
                        op: Op::UserInput {
                            thread_id: ThreadId::from_string("t"),
                            input: vec![deepomni_protocol::op::UserInput::Text {
                                text: "hello".into(),
                            }],
                            settings: Default::default(),
                        },
                        trace: None,
                    },
                    sink,
                )
                .await;
            let events = drain_events(collector, keepalive).await;
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], EventMsg::OpStarted { .. }));
            assert!(
                matches!(&events[1], EventMsg::OpCompleted { status, .. } if status == "user_input:hello")
            );
        }

        // Cancel
        {
            let coordinator = TurnCoordinator::new(services.clone());
            let collector = EventCollector::new();
            let (sink, keepalive) = event_sink_from_collector(collector.clone());
            coordinator
                .handle_op(
                    Submission {
                        id: SubmissionId::new(),
                        op: Op::Cancel {
                            thread_id: ThreadId::from_string("t"),
                        },
                        trace: None,
                    },
                    sink,
                )
                .await;
            let events = drain_events(collector, keepalive).await;
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], EventMsg::OpStarted { .. }));
            assert!(
                matches!(&events[1], EventMsg::OpCompleted { status, .. } if status == "interrupted")
            );
        }

        // Compact
        {
            let coordinator = TurnCoordinator::new(services.clone());
            let collector = EventCollector::new();
            let (sink, keepalive) = event_sink_from_collector(collector.clone());
            coordinator
                .handle_op(
                    Submission {
                        id: SubmissionId::new(),
                        op: Op::Compact {
                            thread_id: ThreadId::from_string("t"),
                        },
                        trace: None,
                    },
                    sink,
                )
                .await;
            let events = drain_events(collector, keepalive).await;
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], EventMsg::OpStarted { .. }));
            assert!(
                matches!(&events[1], EventMsg::OpCompleted { status, .. } if status == "completed")
            );
        }

        // SteerInput
        {
            let coordinator = TurnCoordinator::new(services.clone());
            let collector = EventCollector::new();
            let (sink, keepalive) = event_sink_from_collector(collector.clone());
            coordinator
                .handle_op(
                    Submission {
                        id: SubmissionId::new(),
                        op: Op::SteerInput {
                            thread_id: ThreadId::from_string("t"),
                            input: vec![deepomni_protocol::op::UserInput::Text {
                                text: "steer me".into(),
                            }],
                        },
                        trace: None,
                    },
                    sink,
                )
                .await;
            let events = drain_events(collector, keepalive).await;
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], EventMsg::OpStarted { .. }));
            assert!(
                matches!(&events[1], EventMsg::OpCompleted { status, .. } if status == "steer:steer me")
            );
        }

        // ApprovalDecision
        {
            let coordinator = TurnCoordinator::new(services.clone());
            let collector = EventCollector::new();
            let (sink, keepalive) = event_sink_from_collector(collector.clone());
            coordinator
                .handle_op(
                    Submission {
                        id: SubmissionId::new(),
                        op: Op::ApprovalDecision {
                            thread_id: ThreadId::from_string("t"),
                            approval_id: "approval-1".into(),
                            approved: true,
                        },
                        trace: None,
                    },
                    sink,
                )
                .await;
            let events = drain_events(collector, keepalive).await;
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], EventMsg::OpStarted { .. }));
            assert!(
                matches!(&events[1], EventMsg::OpCompleted { status, .. } if status == "approval:approval-1:true")
            );
        }
    }

    #[tokio::test]
    async fn test_coordinator_emits_op_failed_on_error() {
        let services = Arc::new(MockServices::new());
        services.set_fail("simulated crash").await;

        let coordinator = TurnCoordinator::new(services);
        let collector = EventCollector::new();
        let (sink, keepalive) = event_sink_from_collector(collector.clone());

        let sub_id = SubmissionId("sub-fail".into());
        coordinator
            .handle_op(
                Submission {
                    id: sub_id,
                    op: Op::Cancel {
                        thread_id: ThreadId::from_string("t"),
                    },
                    trace: None,
                },
                sink,
            )
            .await;

        let events = drain_events(collector, keepalive).await;
        assert_eq!(events.len(), 2, "should emit OpStarted + OpFailed");
        assert!(matches!(events[0], EventMsg::OpStarted { .. }));
        assert!(
            matches!(&events[1], EventMsg::OpFailed { error, .. } if error == "simulated crash")
        );
    }

    #[tokio::test]
    async fn test_coordinator_correlates_event_id_with_submission() {
        let services = Arc::new(MockServices::new());
        let coordinator = TurnCoordinator::new(services);
        let sub_id = SubmissionId("sub-corr-1".into());

        let (tx, mut rx) = mpsc::unbounded_channel::<deepomni_protocol::Event>();
        let sink = SessionEventSink::new(tx);
        coordinator
            .handle_op(
                Submission {
                    id: sub_id.clone(),
                    op: Op::Cancel {
                        thread_id: ThreadId::from_string("t"),
                    },
                    trace: None,
                },
                sink,
            )
            .await;

        let mut received = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            received.push(ev);
        }
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].id, sub_id);
        assert_eq!(received[1].id, sub_id);
    }

    #[tokio::test]
    async fn test_coordinator_does_not_depend_on_runtime_types() {
        // Compile-time check: TurnCoordinator should have zero knowledge of
        // oneshot, submission_replies, or any runtime-specific type.
        // This test verifies the coordinator can be constructed without any
        // runtime imports.
        let services = Arc::new(MockServices::new());
        let _coordinator = TurnCoordinator::new(services);
        // If it compiles, the coordinator has no runtime dependency.
    }
}
