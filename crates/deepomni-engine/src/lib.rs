//! Engine-layer session and turn state primitives.
//!
//! This crate owns the decomposition boundary between the SDK-facing runtime
//! facade and the turn machinery. Runtime still delegates most behavior while
//! journal migration settles; these types provide the stable FSM/container
//! surface for moving code out of `deepomni-runtime`.

pub mod approval;
pub mod request_compiler;
pub mod session;
pub mod tool_transaction;
pub mod turn_fsm;

pub use approval::{ApprovalCoordinator, ApprovalOutcome};
pub use request_compiler::EngineRequestCompiler;
pub use session::{Session, SessionId, SessionManager, SessionState};
pub use tool_transaction::ToolTransactionEngine;
pub use turn_fsm::{TurnState, TurnStateMachine};

