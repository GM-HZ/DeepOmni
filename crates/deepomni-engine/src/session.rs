use std::collections::HashMap;

use deepomni_context::ContextManager;
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_tools::ApprovalStore;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

#[derive(Debug, Clone)]
pub struct Session {
    pub id: SessionId,
    pub thread_id: ThreadId,
    pub state: SessionState,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub context_manager: ContextManager,
    pub active_turn_id: Option<TurnId>,
    pub approval_store: std::sync::Arc<ApprovalStore>,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            context_manager: ContextManager::new(),
            active_turn_id: None,
            approval_store: std::sync::Arc::new(ApprovalStore::new()),
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: HashMap<SessionId, Session>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, session: Session) {
        self.sessions.insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_manager_tracks_sessions() {
        let mut manager = SessionManager::new();
        let id = SessionId("s1".into());
        manager.insert(Session {
            id: id.clone(),
            thread_id: ThreadId::new(),
            state: SessionState::new(),
        });

        assert_eq!(manager.len(), 1);
        assert!(manager.get(&id).is_some());
    }
}

