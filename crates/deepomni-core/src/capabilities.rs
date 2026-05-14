//! # DeepOmni Capabilities
//!
//! Centralized feature gate registry. Each capability has a lifecycle
//! stage that controls when and how it's exposed. Pattern from Codex
//! `Feature` enum + Claude Code `feature()` macro.

use serde::{Deserialize, Serialize};

/// Lifecycle stage of a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    UnderDevelopment,
    Experimental,
    Stable,
    Deprecated,
}

/// All optional capabilities in DeepOmni.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    ComputerUse,
    McpServers,
    Plugins,
    SubAgent,
    RemoteWorker,
    Hooks,
    Skills,
    WebSearch,
    LspDiagnostics,
}

impl Capability {
    pub fn stage(&self) -> Stage {
        match self {
            Capability::ComputerUse => Stage::UnderDevelopment,
            Capability::McpServers => Stage::Stable,
            Capability::Plugins => Stage::Stable,
            Capability::SubAgent => Stage::Experimental,
            Capability::RemoteWorker => Stage::UnderDevelopment,
            Capability::Hooks => Stage::Stable,
            Capability::Skills => Stage::Stable,
            Capability::WebSearch => Stage::UnderDevelopment,
            Capability::LspDiagnostics => Stage::Experimental,
        }
    }

    pub fn default_enabled(&self) -> bool {
        matches!(self.stage(), Stage::Stable)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::ComputerUse => "computer-use",
            Capability::McpServers => "mcp-servers",
            Capability::Plugins => "plugins",
            Capability::SubAgent => "sub-agent",
            Capability::RemoteWorker => "remote-worker",
            Capability::Hooks => "hooks",
            Capability::Skills => "skills",
            Capability::WebSearch => "web-search",
            Capability::LspDiagnostics => "lsp-diagnostics",
        }
    }
}

/// Runtime capability registry. Manages which features are enabled.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    enabled: std::collections::HashSet<Capability>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        let mut registry = Self::default();
        for cap in &[
            Capability::McpServers,
            Capability::Plugins,
            Capability::Hooks,
            Capability::Skills,
        ] {
            registry.enable(*cap);
        }
        registry
    }

    pub fn enable(&mut self, cap: Capability) {
        self.enabled.insert(cap);
    }

    pub fn disable(&mut self, cap: Capability) {
        self.enabled.remove(&cap);
    }

    pub fn is_enabled(&self, cap: Capability) -> bool {
        self.enabled.contains(&cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_registry() {
        let reg = CapabilityRegistry::new();
        assert!(reg.is_enabled(Capability::McpServers));
        assert!(!reg.is_enabled(Capability::ComputerUse));
    }

    #[test]
    fn test_enable_disable() {
        let mut reg = CapabilityRegistry::new();
        reg.enable(Capability::ComputerUse);
        assert!(reg.is_enabled(Capability::ComputerUse));
        reg.disable(Capability::ComputerUse);
        assert!(!reg.is_enabled(Capability::ComputerUse));
    }

    #[test]
    fn test_stages() {
        assert_eq!(Capability::ComputerUse.stage(), Stage::UnderDevelopment);
        assert_eq!(Capability::McpServers.stage(), Stage::Stable);
    }
}
