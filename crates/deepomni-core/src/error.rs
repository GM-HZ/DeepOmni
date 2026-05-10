//! DeepOmni error taxonomy.
//!
//! Based on the design in PRD §19.1. Every error carries a stable kind,
//! user-safe message, retryability, visibility, and redaction status.

use std::fmt;

/// Whether an error is safe to retry automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    /// Safe to retry with backoff (e.g., transient provider 5xx, write conflict).
    Retryable,
    /// May be retryable with configuration (e.g., rate limits).
    RetryableWithConfig,
    /// Not safe to retry (e.g., invalid config, policy denial).
    NotRetryable,
}

/// Whether an error message is safe to show to a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Safe to display directly.
    UserVisible,
    /// Internal detail only; show to developers/operators.
    DeveloperVisible,
}

/// Top-level error kind taxonomy. Every DeepOmni error maps to one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Config,
    Protocol,
    Provider,
    Tool,
    PolicyDenied,
    Sandbox,
    State,
    Plugin,
    Mcp,
    Timeout,
    Cancelled,
    RateLimited,
    InvalidModelOutput,
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ErrorKind::Config => "config",
            ErrorKind::Protocol => "protocol",
            ErrorKind::Provider => "provider",
            ErrorKind::Tool => "tool",
            ErrorKind::PolicyDenied => "policy_denied",
            ErrorKind::Sandbox => "sandbox",
            ErrorKind::State => "state",
            ErrorKind::Plugin => "plugin",
            ErrorKind::Mcp => "mcp",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::InvalidModelOutput => "invalid_model_output",
            ErrorKind::Internal => "internal",
        };
        write!(f, "{s}")
    }
}

/// The unified error type for DeepOmni.
///
/// Every public API in the runtime should use this error type or wrap it
/// in a crate-specific type that can convert into it.
#[derive(Debug)]
pub struct DeepOmniError {
    /// Stable error kind for programmatic matching.
    pub kind: ErrorKind,
    /// Safe-for-users message (no secrets, no internal paths).
    pub message: String,
    /// Optional detail for developers/operators (may contain paths).
    pub detail: Option<String>,
    /// Whether retry is safe.
    pub retryability: Retryability,
    /// Whether the message is safe for end users.
    pub visibility: Visibility,
    /// Whether this error has already been redacted.
    pub is_redacted: bool,
    /// The underlying source error, if any.
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl DeepOmniError {
    /// Create a new error with the given kind and user-safe message.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: None,
            retryability: Retryability::NotRetryable,
            visibility: Visibility::UserVisible,
            is_redacted: false,
            source: None,
        }
    }

    /// Set developer-facing detail.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Mark this error as retryable.
    pub fn with_retryable(mut self, r: Retryability) -> Self {
        self.retryability = r;
        self
    }

    /// Mark this error as developer-visible only.
    pub fn with_dev_visible(mut self) -> Self {
        self.visibility = Visibility::DeveloperVisible;
        self
    }

    /// Attach a source error.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Builder: create from a known pattern.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
            .with_visibility(Visibility::DeveloperVisible)
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Protocol, message)
    }

    pub fn provider(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Provider, message)
            .with_retryable(Retryability::RetryableWithConfig)
    }

    pub fn tool(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Tool, message)
    }

    pub fn policy_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::PolicyDenied, message)
    }

    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::RateLimited, message)
            .with_retryable(Retryability::RetryableWithConfig)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Timeout, message)
            .with_retryable(Retryability::RetryableWithConfig)
    }

    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "operation cancelled")
    }

    pub fn invalid_model_output(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidModelOutput, message)
    }

    // Private setter used by builder methods.
    fn with_visibility(mut self, v: Visibility) -> Self {
        self.visibility = v;
        self
    }
}

impl fmt::Display for DeepOmniError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.kind, self.message)?;
        if let Some(ref detail) = self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for DeepOmniError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as _)
    }
}

/// Shorthand for creating errors.
#[macro_export]
macro_rules! bail {
    ($kind:ident, $msg:expr) => {
        return Err($crate::error::DeepOmniError::$kind($msg.into()))
    };
    ($kind:ident, $fmt:expr, $($arg:tt)*) => {
        return Err($crate::error::DeepOmniError::$kind(format!($fmt, $($arg)*)))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_kind_display() {
        assert_eq!(ErrorKind::Config.to_string(), "config");
        assert_eq!(ErrorKind::Provider.to_string(), "provider");
        assert_eq!(ErrorKind::PolicyDenied.to_string(), "policy_denied");
        assert_eq!(ErrorKind::Tool.to_string(), "tool");
    }

    #[test]
    fn test_error_display_no_detail() {
        let err = DeepOmniError::config("missing field").with_retryable(Retryability::NotRetryable);
        let s = err.to_string();
        assert!(s.contains("[config]"));
        assert!(s.contains("missing field"));
    }

    #[test]
    fn test_error_display_with_detail() {
        let err = DeepOmniError::provider("API error")
            .with_detail("POST /chat/completions returned 503");
        let s = err.to_string();
        assert!(s.contains("[provider]"));
        assert!(s.contains("API error"));
        assert!(s.contains("503"));
    }

    #[test]
    fn test_error_is_std_error() {
        let err = DeepOmniError::internal("boom");
        let _: &dyn std::error::Error = &err;
    }

    #[test]
    fn test_error_kind_all_variants() {
        // Ensure all variants construct without panic
        let _ = DeepOmniError::config("test");
        let _ = DeepOmniError::protocol("test");
        let _ = DeepOmniError::provider("test");
        let _ = DeepOmniError::tool("test");
        let _ = DeepOmniError::policy_denied("test");
        let _ = DeepOmniError::rate_limited("test");
        let _ = DeepOmniError::timeout("test");
        let _ = DeepOmniError::cancelled();
        let _ = DeepOmniError::invalid_model_output("test");
        let _ = DeepOmniError::internal("test");
    }
}
