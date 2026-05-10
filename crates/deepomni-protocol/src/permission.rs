//! Permission profile and permission type definitions.

use serde::{Deserialize, Serialize};

/// Permission types used by tools, plugins, and computer-use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PermissionType {
    // ── Filesystem ──
    FilesystemRead,
    FilesystemWrite,

    // ── Network ──
    Network,
    NetworkGithub,
    NetworkGitlab,
    NetworkApi,

    // ── Shell ──
    ShellExec,
    ShellInstall,

    // ── Computer Use (placeholder, disabled by default) ──
    ScreenCapture,
    InputControl,
    WindowInspect,
    ClipboardRead,
    ClipboardWrite,

    // ── Extensibility ──
    /// Custom permission (plugin-defined).
    Other(String),
}

/// A profile grouping permissions for a token or session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionProfile {
    pub allowed: Vec<PermissionType>,
    pub denied: Vec<PermissionType>,
    /// If true, unknown permissions default to denied.
    #[serde(default)]
    pub default_deny: bool,
}

impl PermissionProfile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow(mut self, p: PermissionType) -> Self {
        self.allowed.push(p);
        self
    }

    pub fn deny(mut self, p: PermissionType) -> Self {
        self.denied.push(p);
        self
    }

    pub fn with_default_deny(mut self) -> Self {
        self.default_deny = true;
        self
    }

    /// Check if a permission is explicitly allowed.
    pub fn is_allowed(&self, permission: &PermissionType) -> bool {
        if self.denied.contains(permission) {
            return false;
        }
        if self.allowed.contains(permission) {
            return true;
        }
        !self.default_deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_allow() {
        let profile = PermissionProfile::default();
        assert!(profile.is_allowed(&PermissionType::FilesystemRead));
    }

    #[test]
    fn test_explicit_deny() {
        let profile = PermissionProfile::new().deny(PermissionType::ShellExec);
        assert!(!profile.is_allowed(&PermissionType::ShellExec));
    }

    #[test]
    fn test_default_deny_mode() {
        let profile = PermissionProfile::new()
            .allow(PermissionType::FilesystemRead)
            .with_default_deny();
        assert!(profile.is_allowed(&PermissionType::FilesystemRead));
        assert!(!profile.is_allowed(&PermissionType::Network));
    }

    #[test]
    fn test_permission_serde() {
        let json = r#""network_github""#;
        let p: PermissionType = serde_json::from_str(json).unwrap();
        assert_eq!(p, PermissionType::NetworkGithub);
    }
}
