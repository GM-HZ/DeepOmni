//! # DeepOmni Protocol
//!
//! Stable API and wire types. This crate must not depend on runtime,
//! server, tools, or provider crates. It is the canonical source of
//! truth for serializable types shared across all hosts and transports.

pub mod agent;
pub mod approval;
pub mod error;
pub mod event;
pub mod id;
pub mod op;
pub mod permission;
pub mod sandbox;
pub mod server_message;
pub mod thread;
pub mod tool;
pub mod turn;
pub mod usage;

// Re-exports
pub use approval::{ApprovalDecision, ApprovalRequest};
pub use error::ProtocolError;
pub use event::{Event, EventEnvelope, EventFrame, EventMsg};
pub use id::EventSeq;
pub use id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};
pub use permission::{PermissionProfile, PermissionType};
pub use sandbox::SandboxPolicy;
pub use thread::{CreateThreadRequest, SessionSource, Thread, ThreadStatus};
pub use tool::{ToolOutput, ToolPayload, ToolSchema, ToolSpec};
pub use turn::{CreateTurnRequest, Turn, TurnItem, TurnStatus};
pub use usage::{CostInfo, Usage};
