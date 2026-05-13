//! TurnStateMachine — owns the turn lifecycle from Started to Completed/Failed.
//! This is the single authority for turn state transitions.

use deepomni_protocol::id::{ThreadId, TurnId};

/// Turn lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Started,
    Streaming,
    WaitingForApproval,
    ExecutingTool,
    Completed,
    Failed,
    Interrupted,
}

impl TurnState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TurnState::Completed | TurnState::Failed | TurnState::Interrupted
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TurnState::Started => "started",
            TurnState::Streaming => "streaming",
            TurnState::WaitingForApproval => "waiting_for_approval",
            TurnState::ExecutingTool => "executing_tool",
            TurnState::Completed => "completed",
            TurnState::Failed => "failed",
            TurnState::Interrupted => "interrupted",
        }
    }
}

/// The turn state machine. Tracks a single turn's lifecycle.
#[derive(Clone)]
pub struct TurnStateMachine {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub state: TurnState,
    pub user_input: String,
}

impl TurnStateMachine {
    pub fn new(thread_id: ThreadId, turn_id: TurnId, user_input: String) -> Self {
        Self {
            thread_id,
            turn_id,
            state: TurnState::Started,
            user_input,
        }
    }

    /// Transition to a new state. Returns error if the transition is invalid.
    pub fn transition(&mut self, new_state: TurnState) -> Result<(), TurnFsmError> {
        let valid = match (self.state, new_state) {
            (TurnState::Started, TurnState::Streaming) => true,
            (TurnState::Streaming, TurnState::WaitingForApproval) => true,
            (TurnState::Streaming, TurnState::ExecutingTool) => true,
            (TurnState::Streaming, TurnState::Completed) => true,
            (TurnState::Streaming, TurnState::Failed) => true,
            (TurnState::WaitingForApproval, TurnState::ExecutingTool) => true,
            (TurnState::WaitingForApproval, TurnState::Failed) => true,
            (TurnState::ExecutingTool, TurnState::Streaming) => true, // continue after tool
            (TurnState::ExecutingTool, TurnState::Completed) => true,
            (TurnState::ExecutingTool, TurnState::Failed) => true,
            (_, TurnState::Interrupted) => true,
            (_, s) if s == self.state => true, // re-entrant ok
            _ => false,
        };
        if valid {
            self.state = new_state;
            Ok(())
        } else {
            Err(TurnFsmError::InvalidTransition {
                from: self.state,
                to: new_state,
            })
        }
    }
}

#[derive(Debug)]
pub enum TurnFsmError {
    InvalidTransition { from: TurnState, to: TurnState },
}

impl std::fmt::Display for TurnFsmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnFsmError::InvalidTransition { from, to } => {
                write!(f, "invalid turn transition: {:?} -> {:?}", from, to)
            }
        }
    }
}

impl std::error::Error for TurnFsmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let mut fsm = TurnStateMachine::new(
            ThreadId::from_string("t1"),
            TurnId::from_string("tu1"),
            "hello".into(),
        );
        assert!(fsm.transition(TurnState::Streaming).is_ok());
        assert!(fsm.transition(TurnState::WaitingForApproval).is_ok());
        assert!(fsm.transition(TurnState::ExecutingTool).is_ok());
        assert!(fsm.transition(TurnState::Completed).is_ok());
    }

    #[test]
    fn test_invalid_transition() {
        let mut fsm = TurnStateMachine::new(
            ThreadId::from_string("t1"),
            TurnId::from_string("tu1"),
            "hello".into(),
        );
        // Can't go directly from Started to Completed.
        assert!(fsm.transition(TurnState::Completed).is_err());
    }

    #[test]
    fn test_terminal_states() {
        assert!(TurnState::Completed.is_terminal());
        assert!(TurnState::Failed.is_terminal());
        assert!(!TurnState::Streaming.is_terminal());
    }
}
