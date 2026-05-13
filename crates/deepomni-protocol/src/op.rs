//! Op types — the narrow entry point into the agent core.
//! Pattern from Codex: all external input flows through Op, never through direct method calls.

use crate::id::ThreadId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Unique submission identifier. Returned immediately when a client submits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SubmissionId(pub String);

impl Default for SubmissionId {
    fn default() -> Self {
        Self::new()
    }
}

impl SubmissionId {
    pub fn new() -> Self {
        Self(format!("sub-{}", uuid::Uuid::new_v4().simple()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubmissionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A submission wraps an Op with an id for tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub id: SubmissionId,
    pub op: Op,
}

/// Structured user input — not a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInput {
    Text { text: String },
    Image { image_url: String },
    LocalImage { path: PathBuf },
    Skill { name: String, path: PathBuf },
    Mention { name: String, path: String },
}

/// Per-turn runtime settings submitted alongside user input.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnSettings {
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<String>,
    pub max_token_budget: Option<u64>,
}

/// The single entry point for all external actions into the core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    UserInput {
        thread_id: ThreadId,
        input: Vec<UserInput>,
        settings: TurnSettings,
    },
    SteerInput {
        thread_id: ThreadId,
        input: Vec<UserInput>,
    },
    ApprovalDecision {
        thread_id: ThreadId,
        approval_id: String,
        approved: bool,
    },
    Cancel {
        thread_id: ThreadId,
    },
    Compact {
        thread_id: ThreadId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ThreadId;

    #[test]
    fn test_op_user_input_serializes() {
        let op = Op::UserInput {
            thread_id: ThreadId::from_string("thread-1"),
            input: vec![UserInput::Text {
                text: "hello".into(),
            }],
            settings: TurnSettings {
                model: Some("deepseek-chat".into()),
                cwd: Some("/tmp".into()),
                ..Default::default()
            },
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json["type"], "user_input");
        assert_eq!(json["settings"]["model"], "deepseek-chat");
    }

    #[test]
    fn test_op_approval_serializes() {
        let op = Op::ApprovalDecision {
            thread_id: ThreadId::from_string("t1"),
            approval_id: "approval-1".into(),
            approved: true,
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(json["type"], "approval_decision");
    }

    #[test]
    fn test_submission_id_unique() {
        let a = SubmissionId::new();
        let b = SubmissionId::new();
        assert_ne!(a, b);
    }
}
