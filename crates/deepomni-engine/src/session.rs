//! SessionManager — per-thread session lifecycle. One session = one thread.
//! Phase B: Uses real SessionLoopHandle instead of dummy channels.
//! The canonical Op type is `deepomni_protocol::op::Op`.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::session_loop::{SessionLoopError, SessionLoopHandle};
use deepomni_protocol::id::ThreadId;
use deepomni_protocol::op::{Op, SubmissionId, W3cTraceContext};

/// Manages active sessions, one per thread.
pub struct SessionManager {
    sessions: RwLock<HashMap<ThreadId, SessionHandle>>,
}

/// Handle to an active session. Holds the background SessionLoopHandle.
#[derive(Clone)]
pub struct SessionHandle {
    pub thread_id: ThreadId,
    pub session_loop: Arc<SessionLoopHandle>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new session for a thread with its SessionLoopHandle.
    pub async fn register(&self, thread_id: ThreadId, session_loop: Arc<SessionLoopHandle>) {
        self.sessions.write().await.insert(
            thread_id.clone(),
            SessionHandle {
                thread_id,
                session_loop,
            },
        );
    }

    /// Remove a session when the thread is archived.
    pub async fn remove(&self, thread_id: &ThreadId) {
        self.sessions.write().await.remove(thread_id);
    }

    /// Check if a thread has an active session.
    pub async fn has(&self, thread_id: &ThreadId) -> bool {
        self.sessions.read().await.contains_key(thread_id)
    }

    /// Get the SessionLoopHandle for a thread, if registered.
    pub async fn get_handle(&self, thread_id: &ThreadId) -> Option<Arc<SessionLoopHandle>> {
        self.sessions
            .read()
            .await
            .get(thread_id)
            .map(|h| Arc::clone(&h.session_loop))
    }

    /// Submit an Op to the session's loop and return a SubmissionId.
    pub async fn submit_to(
        &self,
        thread_id: &ThreadId,
        op: Op,
    ) -> Result<SubmissionId, SessionLoopError> {
        self.submit_to_with_trace(thread_id, op, None).await
    }

    /// Submit an Op with trace context to the session's loop and return a SubmissionId.
    pub async fn submit_to_with_trace(
        &self,
        thread_id: &ThreadId,
        op: Op,
        trace: Option<W3cTraceContext>,
    ) -> Result<SubmissionId, SessionLoopError> {
        let guard = self.sessions.read().await;
        let handle = guard.get(thread_id).ok_or(SessionLoopError::Closed)?;
        let id = SubmissionId::new();
        handle
            .session_loop
            .submit_with_id_and_trace(id.clone(), op, trace)?;
        Ok(id)
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpHandler;
    use crate::session_loop::SessionLoop;
    use tokio::sync::mpsc;

    struct TestHandler {
        tx: mpsc::UnboundedSender<String>,
    }

    #[async_trait::async_trait]
    impl OpHandler for TestHandler {
        async fn handle_op(&self, submission: deepomni_protocol::op::Submission) {
            let _ = self.tx.send(match &submission.op {
                Op::Cancel { .. } => "cancel".into(),
                Op::Compact { .. } => "compact".into(),
                _ => "other".into(),
            });
        }
    }

    #[tokio::test]
    async fn test_session_manager_register_and_submit() {
        let mgr = SessionManager::new();
        let tid = ThreadId::from_string("test-session");

        let (tx, mut rx) = mpsc::unbounded_channel();
        let handler = Arc::new(TestHandler { tx });
        let loop_handle = Arc::new(SessionLoop::spawn(tid.clone(), handler));

        mgr.register(tid.clone(), loop_handle).await;
        assert!(mgr.has(&tid).await);

        // Submit through the manager.
        let sub_id = mgr
            .submit_to(
                &tid,
                Op::Cancel {
                    thread_id: tid.clone(),
                },
            )
            .await
            .unwrap();
        assert!(!sub_id.as_str().is_empty());

        // Give the handler time to process.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, "cancel");

        // Remove the session.
        mgr.remove(&tid).await;
        assert!(!mgr.has(&tid).await);
    }
}
