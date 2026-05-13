//! Inter-agent communication types. Based on Codex InterAgentCommunication pattern.

use crate::id::ThreadId;
use serde::{Deserialize, Serialize};

/// Path-based agent identifier in the agent tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct AgentPath(pub String);

impl AgentPath {
    pub fn root() -> Self {
        Self("/root".into())
    }
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A message between agents. Published through the Mailbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterAgentMessage {
    pub author: AgentPath,
    pub recipient: AgentPath,
    #[serde(default)]
    pub other_recipients: Vec<AgentPath>,
    pub content: String,
    /// Whether receiving this message triggers a new turn.
    pub trigger_turn: bool,
}

/// Public metadata about an agent visible to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub thread_id: ThreadId,
    pub agent_path: AgentPath,
    pub status: AgentStatus,
    pub task: Option<String>,
}

/// Agent lifecycle status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl AgentStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inter_agent_message_serializes() {
        let msg = InterAgentMessage {
            author: AgentPath::root(),
            recipient: AgentPath::from_string("/root/worker"),
            other_recipients: Vec::new(),
            content: "done".into(),
            trigger_turn: true,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["trigger_turn"], true);
        assert_eq!(json["author"], "/root");
    }

    #[test]
    fn test_agent_status_terminal() {
        assert!(AgentStatus::Completed.is_terminal());
        assert!(!AgentStatus::Running.is_terminal());
    }
}
