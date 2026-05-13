//! Protocol-level error types.
//!
//! These are errors related to protocol serialization/deserialization and
//! message-level validation, NOT runtime execution errors (those are in deepomni-core).

use serde::{Deserialize, Serialize};

/// Errors at the protocol/wire level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProtocolError {
    InvalidEventTag { tag: String },
    MissingField { field: String },
    InvalidId { id_type: String, value: String },
    SchemaViolation { details: String },
    BackwardCompatibilityViolation { field: String },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::InvalidEventTag { tag } => {
                write!(f, "invalid event tag: {tag}")
            }
            ProtocolError::MissingField { field } => {
                write!(f, "missing required field: {field}")
            }
            ProtocolError::InvalidId { id_type, value } => {
                write!(f, "invalid {id_type}: {value}")
            }
            ProtocolError::SchemaViolation { details } => {
                write!(f, "schema violation: {details}")
            }
            ProtocolError::BackwardCompatibilityViolation { field } => {
                write!(
                    f,
                    "backward compatibility violation: field '{field}' changed incompatibly"
                )
            }
        }
    }
}

impl std::error::Error for ProtocolError {}
