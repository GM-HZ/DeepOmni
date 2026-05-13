//! # DeepOmni Engine
//!
//! Session/Turn state machine, RequestCompiler, and approval coordination.
//! The engine is the single authority for turn lifecycle.

pub mod agent_control;
pub mod approval;
pub mod compaction;
pub mod mailbox;
pub mod request_compiler;
pub mod session;
pub mod session_loop;
pub mod turn_coordinator;
pub mod turn_fsm;

pub use agent_control::{AgentControl, AgentRegistry};
pub use approval::ApprovalCoordinator;
pub use compaction::CompactionTracker;
pub use mailbox::{Mailbox, MailboxReceiver};
pub use request_compiler::{CompileConfig, CompiledRequest, RequestCompiler};
pub use session::SessionManager;
pub use session_loop::{OpHandler, SessionLoop, SessionLoopHandle};
pub use turn_coordinator::{TurnCoordinator, TurnOpResult, TurnServices};
pub use turn_fsm::{TurnFsmError, TurnState, TurnStateMachine};
