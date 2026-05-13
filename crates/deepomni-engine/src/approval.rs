//! ApprovalCoordinator — owns the approval lifecycle.

use std::collections::HashMap;
use tokio::sync::RwLock;

use deepomni_protocol::id::{ThreadId, ToolCallId, TurnId};

/// A pending approval record, durably stored.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub approval_id: String,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub model: String,
    pub workspace: String,
    pub config_json: String,
}

/// Coordinates the approval lifecycle. Reads from durable state
/// and manages in-memory pending approvals.
pub struct ApprovalCoordinator {
    pending: RwLock<HashMap<String, PendingApproval>>,
}

impl ApprovalCoordinator {
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new pending approval (called from NeedsApproval path).
    pub async fn register(&self, approval: PendingApproval) {
        self.pending
            .write()
            .await
            .insert(approval.approval_id.clone(), approval);
    }

    /// Resolve a pending approval by turn. Returns None if not found.
    pub async fn resolve_by_turn(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
    ) -> Option<PendingApproval> {
        let mut guard = self.pending.write().await;
        let key = guard
            .iter()
            .find(|(_, v)| &v.thread_id == thread_id && &v.turn_id == turn_id)
            .map(|(k, _)| k.clone());
        key.and_then(|k| guard.remove(&k))
    }

    /// Check if a thread has any pending approvals.
    pub async fn has_pending(&self, thread_id: &ThreadId) -> bool {
        self.pending
            .read()
            .await
            .values()
            .any(|v| &v.thread_id == thread_id)
    }

    /// Resolve a pending approval by its approval_id (not turn_id).
    pub async fn resolve_by_approval_id(&self, approval_id: &str) -> Option<PendingApproval> {
        let mut guard = self.pending.write().await;
        guard.remove(approval_id)
    }
}

impl Default for ApprovalCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_protocol::id::ToolCallId;

    #[tokio::test]
    async fn test_approval_register_and_resolve() {
        let coord = ApprovalCoordinator::new();
        let tid = ThreadId::from_string("thread-1");
        let turn_id = TurnId::from_string("turn-1");

        coord
            .register(PendingApproval {
                approval_id: "approval-1".into(),
                thread_id: tid.clone(),
                turn_id: turn_id.clone(),
                call_id: ToolCallId::from_string("call-1"),
                tool_name: "write_file".into(),
                arguments: serde_json::json!({"path": "test.txt"}),
                model: "deepseek-chat".into(),
                workspace: "/workspace".into(),
                config_json: "{}".into(),
            })
            .await;

        let resolved = coord.resolve_by_turn(&tid, &turn_id).await.unwrap();
        assert_eq!(resolved.approval_id, "approval-1");
        assert_eq!(resolved.tool_name, "write_file");
    }
}
