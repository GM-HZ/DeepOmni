//! # DeepOmni Core
//!
//! Foundation crate providing the shared error taxonomy, `Result` alias,
//! retryability and visibility markers, and redaction helpers.
//!
//! This crate is intentionally minimal. It must not accumulate domain logic.
//! If a type is not a shared primitive needed by more than 3 crates, it
//! belongs in a domain crate.

pub mod error;
pub mod redact;

pub use error::{DeepOmniError, ErrorKind};
pub use redact::Redactable;

/// Convenience alias for results using [`DeepOmniError`].
pub type Result<T> = std::result::Result<T, DeepOmniError>;
