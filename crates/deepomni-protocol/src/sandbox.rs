//! Sandbox policy types.

use serde::{Deserialize, Serialize};

/// Sandbox mode for tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// No sandboxing.
    None,
    /// macOS Seatbelt sandbox.
    #[cfg(target_os = "macos")]
    Seatbelt,
    /// Linux Landlock sandbox.
    #[cfg(target_os = "linux")]
    Landlock,
    /// Best available platform sandbox.
    Auto,
}

/// Policy controls for sandbox selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxPolicy {
    #[serde(default = "default_sandbox_mode")]
    pub mode: SandboxMode,
    /// Always sandbox mutating tools, even in YOLO mode.
    #[serde(default)]
    pub always_sandbox_mutating: bool,
    /// Allow configuring which paths are accessible.
    #[serde(default)]
    pub allowed_paths: Vec<String>,
    /// Allow configuring which paths are read-only.
    #[serde(default)]
    pub readonly_paths: Vec<String>,
    /// Allow configuring network access within sandbox.
    #[serde(default)]
    pub allow_network: bool,
}

fn default_sandbox_mode() -> SandboxMode {
    SandboxMode::Auto
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            mode: default_sandbox_mode(),
            always_sandbox_mutating: true,
            allowed_paths: vec![],
            readonly_paths: vec![],
            allow_network: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = SandboxPolicy::default();
        assert_eq!(policy.mode, SandboxMode::Auto);
        assert!(policy.always_sandbox_mutating);
    }

    #[test]
    fn test_sandbox_mode_serde() {
        let json = r#""auto""#;
        let mode: SandboxMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, SandboxMode::Auto);
    }
}
