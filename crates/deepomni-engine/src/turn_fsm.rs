#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    Created,
    Streaming,
    AwaitingApproval,
    Continuing,
    Completed,
    Failed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TurnStateError {
    #[error("invalid turn state transition: {from:?} -> {to:?}")]
    InvalidTransition { from: TurnState, to: TurnState },
}

#[derive(Debug, Clone)]
pub struct TurnStateMachine {
    state: TurnState,
}

impl TurnStateMachine {
    pub fn new() -> Self {
        Self {
            state: TurnState::Created,
        }
    }

    pub fn state(&self) -> TurnState {
        self.state
    }

    pub fn transition(&mut self, to: TurnState) -> Result<(), TurnStateError> {
        if is_valid(self.state, to) {
            self.state = to;
            Ok(())
        } else {
            Err(TurnStateError::InvalidTransition {
                from: self.state,
                to,
            })
        }
    }
}

impl Default for TurnStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

fn is_valid(from: TurnState, to: TurnState) -> bool {
    matches!(
        (from, to),
        (TurnState::Created, TurnState::Streaming)
            | (TurnState::Streaming, TurnState::AwaitingApproval)
            | (TurnState::Streaming, TurnState::Completed)
            | (TurnState::Streaming, TurnState::Failed)
            | (TurnState::AwaitingApproval, TurnState::Continuing)
            | (TurnState::AwaitingApproval, TurnState::Failed)
            | (TurnState::Continuing, TurnState::Streaming)
            | (TurnState::Continuing, TurnState::Completed)
            | (TurnState::Continuing, TurnState::Failed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsm_accepts_approval_resume_path() {
        let mut fsm = TurnStateMachine::new();
        fsm.transition(TurnState::Streaming).unwrap();
        fsm.transition(TurnState::AwaitingApproval).unwrap();
        fsm.transition(TurnState::Continuing).unwrap();
        fsm.transition(TurnState::Streaming).unwrap();
        fsm.transition(TurnState::Completed).unwrap();
        assert_eq!(fsm.state(), TurnState::Completed);
    }

    #[test]
    fn fsm_rejects_terminal_restart() {
        let mut fsm = TurnStateMachine::new();
        fsm.transition(TurnState::Streaming).unwrap();
        fsm.transition(TurnState::Completed).unwrap();
        assert!(fsm.transition(TurnState::Streaming).is_err());
    }
}

