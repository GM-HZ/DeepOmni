//! Strongly-typed identifiers for all DeepOmni domain entities.
//!
//! Adapted from DeepSeek-TUI and Codex patterns.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident, $prefix:expr) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generate a new unique ID.
            pub fn new() -> Self {
                Self(format!("{}-{}", $prefix, Uuid::new_v4().simple()))
            }

            /// Create from an existing string.
            pub fn from_string(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_type!(ThreadId, "thread");
id_type!(TurnId, "turn");
id_type!(ToolCallId, "call");
id_type!(MessageId, "msg");
id_type!(SubagentId, "subagent");

/// Monotonically increasing event sequence number within a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct EventSeq(u64);

impl EventSeq {
    pub const fn new(seq: u64) -> Self {
        Self(seq)
    }

    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for EventSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_id_new_format() {
        let id = ThreadId::new();
        assert!(id.as_str().starts_with("thread-"));
    }

    #[test]
    fn test_turn_id_new_format() {
        let id = TurnId::new();
        assert!(id.as_str().starts_with("turn-"));
    }

    #[test]
    fn test_thread_id_json_roundtrip() {
        let id = ThreadId::from_string("thread-abc123");
        let json = serde_json::to_string(&id).unwrap();
        let parsed: ThreadId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn test_event_seq_monotonic() {
        let a = EventSeq::new(0);
        assert!(a.next() > a);
    }

    #[test]
    fn test_id_display() {
        let id = ThreadId::from_string("thread-abc");
        assert_eq!(format!("{id}"), "thread-abc");
    }
}
