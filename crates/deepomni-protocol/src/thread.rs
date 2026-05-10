//! Thread types and lifecycle.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::ThreadId;

/// Lifecycle state of a thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Running,
    Idle,
    Completed,
    Failed,
    Paused,
    Archived,
}

/// How the thread was created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Interactive,
    Resume,
    Fork,
    Api,
    Unknown,
}

/// A conversation thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: ThreadId,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: ThreadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub cwd: PathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
}

/// Parameters for creating a new thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateThreadRequest {
    pub workspace: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<ThreadId>,
    #[serde(default)]
    pub ephemeral: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_status_serde() {
        let json = r#""running""#;
        let s: ThreadStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s, ThreadStatus::Running);
        assert_eq!(serde_json::to_string(&s).unwrap(), json);
    }

    #[test]
    fn test_session_source_serde() {
        let json = r#""interactive""#;
        let s: SessionSource = serde_json::from_str(json).unwrap();
        assert_eq!(s, SessionSource::Interactive);
    }
}
