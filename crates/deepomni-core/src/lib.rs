//! # DeepOmni Core
//!
//! Foundation crate providing shared primitives: error types, capability
//! registry, trace writer trait, and redaction helpers.

pub mod capabilities;
pub mod error;
pub mod redact;

pub use capabilities::{Capability, CapabilityRegistry, Stage};
pub use error::{DeepOmniError, ErrorKind};
pub use redact::Redactable;

/// Convenience alias for results using [`DeepOmniError`].
pub type Result<T> = std::result::Result<T, DeepOmniError>;
