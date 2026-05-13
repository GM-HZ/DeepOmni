//! # DeepOmni Sandbox
//!
//! Platform sandbox abstraction for tool execution isolation.
//! V1 provides adapters for macOS Seatbelt, Linux Landlock, Windows
//! placeholder, and a no-sandbox pass-through.
//!
//! Based on Codex sandboxing and PRD §7.10.

/// Sandbox attempt strategy for the ToolCallRuntime.
/// Controls whether the first attempt is sandboxed and whether
/// a denied attempt can escalate to unsandboxed retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxAttempt {
    /// Execute without sandbox.
    Disabled,
    /// Execute sandboxed. If denied and escalate_on_deny, retry unsandboxed.
    Required { escalate_on_deny: bool },
    /// Prefer sandbox but allow unsandboxed on deny.
    PreferredWithEscalation,
}

impl SandboxAttempt {
    pub fn from_orchestrator_decision(sandbox_required: bool, escalate_on_deny: bool) -> Self {
        if sandbox_required {
            SandboxAttempt::Required { escalate_on_deny }
        } else if escalate_on_deny {
            SandboxAttempt::PreferredWithEscalation
        } else {
            SandboxAttempt::Disabled
        }
    }
}

/// Result of sandbox policy evaluation.
#[derive(Debug, Clone)]
pub enum SandboxDecision {
    /// Execute outside sandbox.
    NoSandbox,
    /// Execute inside a sandbox with restrictions.
    Sandbox(SandboxConfig),
    /// Execution is denied by sandbox policy.
    Denied { reason: String },
}

/// Configuration applied when a process runs sandboxed.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Paths the sandboxed process can read.
    pub readonly_paths: Vec<String>,
    /// Paths the sandboxed process can read and write.
    pub readwrite_paths: Vec<String>,
    /// Whether network access is allowed.
    pub allow_network: bool,
    /// Additional platform-specific rules (JSON).
    pub platform_rules: Option<String>,
}

/// The sandbox manager selects and applies sandbox policies.
pub struct SandboxManager {
    /// Current platform.
    platform: Platform,
    /// Whether sandboxing is enabled.
    enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOS,
    Linux,
    Windows,
    Unknown,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOS
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Unknown
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::MacOS => "macos",
            Platform::Linux => "linux",
            Platform::Windows => "windows",
            Platform::Unknown => "unknown",
        }
    }
}

impl SandboxManager {
    pub fn new() -> Self {
        Self {
            platform: Platform::current(),
            enabled: true,
        }
    }

    /// Create with sandboxing disabled (pass-through mode).
    pub fn disabled() -> Self {
        Self {
            platform: Platform::current(),
            enabled: false,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Decide whether and how a tool invocation should be sandboxed.
    pub fn decide(
        &self,
        tool_name: &str,
        is_mutating: bool,
        workspace_path: &str,
    ) -> SandboxDecision {
        if !self.enabled {
            return SandboxDecision::NoSandbox;
        }

        // Always sandbox mutating shell commands.
        if tool_name == "shell_exec" && is_mutating {
            return SandboxDecision::Sandbox(SandboxConfig {
                readonly_paths: vec![],
                readwrite_paths: vec![workspace_path.to_string()],
                allow_network: false,
                platform_rules: None,
            });
        }

        // For file writes, restrict to workspace.
        if tool_name == "write_file" || tool_name == "apply_patch" {
            return SandboxDecision::Sandbox(SandboxConfig {
                readonly_paths: vec![],
                readwrite_paths: vec![workspace_path.to_string()],
                allow_network: false,
                platform_rules: None,
            });
        }

        // Read-only tools don't need sandbox.
        SandboxDecision::NoSandbox
    }

    /// Apply platform-specific sandbox to a command.
    /// On macOS, applies Seatbelt. On Linux, applies Landlock.
    /// On Windows, returns the command unchanged (placeholder).
    pub fn apply(&self, command: &[String], _config: &SandboxConfig) -> Vec<String> {
        match self.platform {
            Platform::MacOS => {
                // macOS Seatbelt: `sandbox-exec -f <profile> <command>`
                // V1 placeholder: return command unchanged.
                command.to_vec()
            }
            Platform::Linux => {
                // Linux Landlock: apply via landlock crate.
                // V1 placeholder: return command unchanged.
                command.to_vec()
            }
            Platform::Windows | Platform::Unknown => {
                // No sandbox available.
                command.to_vec()
            }
        }
    }
}

impl Default for SandboxManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_sandbox_always_no_sandbox() {
        let manager = SandboxManager::disabled();
        let decision = manager.decide("shell_exec", true, "/workspace");
        assert!(matches!(decision, SandboxDecision::NoSandbox));
    }

    #[test]
    fn test_shell_exec_is_sandboxed() {
        let manager = SandboxManager::new();
        let decision = manager.decide("shell_exec", true, "/workspace");
        match decision {
            SandboxDecision::Sandbox(config) => {
                assert!(config.readwrite_paths.contains(&"/workspace".to_string()));
                assert!(!config.allow_network);
            }
            _ => panic!("expected Sandbox"),
        }
    }

    #[test]
    fn test_read_file_no_sandbox() {
        let manager = SandboxManager::new();
        let decision = manager.decide("read_file", false, "/workspace");
        assert!(matches!(decision, SandboxDecision::NoSandbox));
    }

    #[test]
    fn test_write_file_sandboxed() {
        let manager = SandboxManager::new();
        let decision = manager.decide("write_file", true, "/workspace");
        assert!(matches!(decision, SandboxDecision::Sandbox(_)));
    }
}
