//! Per-thread background session loop. Consumes Submissions and dispatches
//! to the configured op handler. Pattern from Codex Session::submission_loop.

use std::sync::Arc;
use tokio::sync::mpsc;

use deepomni_protocol::id::ThreadId;
use deepomni_protocol::op::{Op, Submission, SubmissionId, W3cTraceContext};
use deepomni_protocol::{Event, EventMsg};

/// Trait for processing ops consumed by the session loop.
/// Implemented by Runtime to wire actual turn execution.
#[async_trait::async_trait]
pub trait OpHandler: Send + Sync + 'static {
    async fn handle_op(&self, submission: Submission);
}

/// Handle for submitting ops to a session loop.
#[derive(Clone)]
pub struct SessionLoopHandle {
    pub thread_id: ThreadId,
    submission_tx: mpsc::UnboundedSender<Submission>,
}

impl SessionLoopHandle {
    /// Submit an op. Returns immediately with a SubmissionId.
    pub fn submit(&self, op: Op) -> Result<SubmissionId, SessionLoopError> {
        let id = SubmissionId::new();
        self.submit_with_id(id.clone(), op)?;
        Ok(id)
    }

    /// Submit an op with a pre-generated SubmissionId. Used when the caller
    /// needs to register reply channels before the handler runs.
    pub fn submit_with_id(&self, id: SubmissionId, op: Op) -> Result<(), SessionLoopError> {
        self.submit_with_id_and_trace(id, op, None)
    }

    /// Submit an op with a pre-generated SubmissionId and optional trace context.
    pub fn submit_with_id_and_trace(
        &self,
        id: SubmissionId,
        op: Op,
        trace: Option<W3cTraceContext>,
    ) -> Result<(), SessionLoopError> {
        self.submission_tx
            .send(Submission { id, op, trace })
            .map_err(|_| SessionLoopError::Closed)
    }
}

/// Receiver side of the per-session event queue.
pub struct SessionEventReceiver {
    event_rx: mpsc::UnboundedReceiver<Event>,
}

impl SessionEventReceiver {
    pub async fn recv(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }

    pub fn try_recv(&mut self) -> Result<Event, mpsc::error::TryRecvError> {
        self.event_rx.try_recv()
    }
}

/// Background session loop. Runs in a tokio task, consuming submissions
/// and dispatching them to the configured OpHandler.
pub struct SessionLoop {
    #[allow(dead_code)]
    thread_id: ThreadId,
    submission_rx: mpsc::UnboundedReceiver<Submission>,
    event_tx: mpsc::UnboundedSender<Event>,
}

impl SessionLoop {
    /// Spawn a new session loop with an op handler. Returns a handle for submitting ops.
    /// The handler is called sequentially for each submission, guaranteeing per-thread ordering.
    pub fn spawn(thread_id: ThreadId, handler: Arc<dyn OpHandler>) -> SessionLoopHandle {
        Self::spawn_with_event_queue(thread_id, handler).0
    }

    /// Spawn a session loop and return both the submission handle and event queue receiver.
    pub fn spawn_with_event_queue(
        thread_id: ThreadId,
        handler: Arc<dyn OpHandler>,
    ) -> (SessionLoopHandle, SessionEventReceiver) {
        let (submission_tx, submission_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let handle = SessionLoopHandle {
            thread_id: thread_id.clone(),
            submission_tx,
        };
        let mut loop_ = Self {
            thread_id,
            submission_rx,
            event_tx,
        };

        tokio::spawn(async move {
            while let Some(submission) = loop_.submission_rx.recv().await {
                let submission_id = submission.id.clone();
                let thread_id = op_thread_id(&submission.op);
                loop_.emit(Event {
                    id: submission_id.clone(),
                    msg: EventMsg::SubmissionStarted {
                        thread_id: thread_id.clone(),
                    },
                });
                handler.handle_op(submission).await;
                loop_.emit(Event {
                    id: submission_id,
                    msg: EventMsg::SubmissionCompleted { thread_id },
                });
            }
        });

        (handle, SessionEventReceiver { event_rx })
    }

    /// Spawn a test loop with an observed channel instead of a real handler.
    pub fn spawn_test() -> (SessionLoopHandle, mpsc::UnboundedReceiver<Submission>) {
        let (submission_tx, submission_rx) = mpsc::unbounded_channel();
        let (observed_tx, observed_rx) = mpsc::unbounded_channel();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();

        let handle = SessionLoopHandle {
            thread_id: ThreadId::from_string("test-thread"),
            submission_tx,
        };
        let mut loop_ = Self {
            thread_id: ThreadId::from_string("test-thread"),
            submission_rx,
            event_tx,
        };

        tokio::spawn(async move {
            while let Some(submission) = loop_.submission_rx.recv().await {
                let _ = observed_tx.send(submission);
            }
        });

        (handle, observed_rx)
    }

    fn emit(&self, event: Event) {
        let _ = self.event_tx.send(event);
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

#[derive(Debug)]
pub enum SessionLoopError {
    Closed,
}

impl std::fmt::Display for SessionLoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionLoopError::Closed => write!(f, "session loop closed"),
        }
    }
}

impl std::error::Error for SessionLoopError {}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::op::Op;

    #[tokio::test]
    async fn test_session_loop_accepts_ordered_submissions() {
        let (handle, mut observed) = SessionLoop::spawn_test();
        let first = handle
            .submit(Op::Cancel {
                thread_id: ThreadId::from_string("t"),
            })
            .unwrap();
        let second = handle
            .submit(Op::Compact {
                thread_id: ThreadId::from_string("t"),
            })
            .unwrap();
        assert_ne!(first.as_str(), second.as_str());

        let sub1 = observed.recv().await.unwrap();
        let sub2 = observed.recv().await.unwrap();
        assert_eq!(sub1.id, first);
        assert_eq!(sub2.id, second);
    }

    #[tokio::test]
    async fn test_submit_returns_immediately() {
        let (handle, mut observed) = SessionLoop::spawn_test();
        let id = handle
            .submit(Op::Cancel {
                thread_id: ThreadId::from_string("t"),
            })
            .unwrap();
        assert!(!id.as_str().is_empty());
        let sub = observed.recv().await.unwrap();
        assert_eq!(sub.id, id);
    }

    #[tokio::test]
    async fn test_submit_with_trace_preserves_trace_context() {
        let (handle, mut observed) = SessionLoop::spawn_test();
        let id = SubmissionId("sub-traced".into());
        handle
            .submit_with_id_and_trace(
                id.clone(),
                Op::Cancel {
                    thread_id: ThreadId::from_string("t"),
                },
                Some(W3cTraceContext {
                    traceparent: Some(
                        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00".into(),
                    ),
                    tracestate: None,
                }),
            )
            .unwrap();

        let sub = observed.recv().await.unwrap();
        assert_eq!(sub.id, id);
        assert_eq!(
            sub.trace.unwrap().traceparent.as_deref(),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00")
        );
    }

    /// A handler that collects submissions into a Vec for test verification.
    struct CollectingHandler {
        tx: mpsc::UnboundedSender<Submission>,
    }

    #[async_trait::async_trait]
    impl OpHandler for CollectingHandler {
        async fn handle_op(&self, submission: Submission) {
            let _ = self.tx.send(submission);
        }
    }

    #[tokio::test]
    async fn test_session_loop_with_handler_processes_in_order() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handler = Arc::new(CollectingHandler { tx });
        let handle = SessionLoop::spawn(ThreadId::from_string("t"), handler);

        handle
            .submit(Op::Cancel {
                thread_id: ThreadId::from_string("t"),
            })
            .unwrap();
        handle
            .submit(Op::Compact {
                thread_id: ThreadId::from_string("t"),
            })
            .unwrap();

        // Give the handler time to process.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut ids = Vec::new();
        while let Ok(sub) = rx.try_recv() {
            ids.push(sub.id);
        }
        assert_eq!(
            ids.len(),
            2,
            "both submissions should be processed in order"
        );
    }

    #[tokio::test]
    async fn test_session_loop_emits_started_and_completed_events() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let handler = Arc::new(CollectingHandler { tx });
        let (handle, mut events) =
            SessionLoop::spawn_with_event_queue(ThreadId::from_string("loop-thread"), handler);

        let submission_id = handle
            .submit(Op::Cancel {
                thread_id: ThreadId::from_string("loop-thread"),
            })
            .unwrap();

        let started = events.recv().await.unwrap();
        let completed = events.recv().await.unwrap();

        assert_eq!(started.id, submission_id);
        assert_eq!(completed.id, submission_id);
        assert!(matches!(
            started.msg,
            EventMsg::SubmissionStarted { ref thread_id } if thread_id.as_str() == "loop-thread"
        ));
        assert!(matches!(
            completed.msg,
            EventMsg::SubmissionCompleted { ref thread_id } if thread_id.as_str() == "loop-thread"
        ));
    }
}
