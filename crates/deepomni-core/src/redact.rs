//! Redaction helpers for secrets, API keys, and sensitive paths.

/// Trait for types that can produce a redacted representation.
pub trait Redactable {
    /// Return a redacted version safe for display/logging.
    fn redacted(&self) -> String;
}

/// Redact a string that may contain an API key.
///
/// Keeps the first 4 and last 4 characters, replacing the middle with `...`.
/// Falls back to `***` for very short strings.
pub fn redact_api_key(value: &str) -> String {
    if value.len() <= 8 {
        return "***".to_string();
    }
    let prefix = &value[..4];
    let suffix = &value[value.len() - 4..];
    format!("{prefix}...{suffix}")
}

/// Redact an environment variable value. Returns `"<set>"` if non-empty.
pub fn redact_env_var(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_string()
    } else {
        "<set>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_api_key() {
        assert_eq!(redact_api_key("sk-abcdefgh1234"), "sk-a...1234");
        assert_eq!(redact_api_key("short"), "***");
    }

    #[test]
    fn test_redact_env_var() {
        assert_eq!(redact_env_var("secret123"), "<set>");
        assert_eq!(redact_env_var(""), "<empty>");
    }
}
