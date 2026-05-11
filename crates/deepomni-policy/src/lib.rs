//! # DeepOmni Policy
//!
//! Approval, permission, network, and execution policy decisions.
//! Every mutating tool call passes through the policy engine before execution.
//!
//! Based on PRD §7.9, Codex tool orchestrator, and DeepSeek-TUI execpolicy.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use deepomni_protocol::permission::PermissionType;

/// Agent execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentMode {
    /// Read-only exploration, no tool execution allowed.
    Plan,
    /// Normal mode: mutating tools require approval.
    Agent,
    /// Auto-approve all tools.
    Trusted,
}

impl AgentMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentMode::Plan => "plan",
            AgentMode::Agent => "agent",
            AgentMode::Trusted => "trusted",
        }
    }
}

/// Approval requirement for a tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalRequirement {
    /// Execute immediately, no approval needed.
    Skip,
    /// Host must approve before execution.
    NeedsApproval {
        reason: String,
        approval_id: String,
    },
    /// Hard-blocked; cannot be approved.
    Forbidden { reason: String },
}

/// Permission profile for an agent or turn.
///
/// Merges the runtime's base profile with per-thread and per-plugin
/// profiles. Checks use explicit allowlists with optional default-deny.
#[derive(Debug, Clone)]
pub struct PermissionProfile {
    pub allowed: HashSet<PermissionType>,
    pub denied: HashSet<PermissionType>,
    pub default_deny: bool,
}

impl PermissionProfile {
    pub fn new() -> Self {
        Self {
            allowed: HashSet::new(),
            denied: HashSet::new(),
            default_deny: false,
        }
    }

    pub fn allow(mut self, p: PermissionType) -> Self {
        self.allowed.insert(p);
        self
    }

    pub fn deny(mut self, p: PermissionType) -> Self {
        self.denied.insert(p);
        self
    }

    pub fn with_default_deny(mut self) -> Self {
        self.default_deny = true;
        self
    }

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

impl Default for PermissionProfile {
    fn default() -> Self {
        Self::new()
    }
}

/// Network policy for tool network access.
#[derive(Debug, Clone)]
pub enum NetworkPolicy {
    /// All network access allowed.
    AllowAll,
    /// Only allowed hosts.
    Allowlisted(Vec<String>),
    /// No network access.
    DenyAll,
    /// Prompt user for each new host.
    PromptPerHost {
        approved_hosts: HashSet<String>,
    },
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        NetworkPolicy::PromptPerHost {
            approved_hosts: HashSet::new(),
        }
    }
}

/// A single policy decision returned by the engine.
#[derive(Debug, Clone)]
pub struct PolicyDecision {
    pub approval: ApprovalRequirement,
    pub sandbox_required: bool,
    pub network_policy: NetworkPolicy,
}

/// The policy engine.
///
/// Evaluates tool calls against the current mode, permission profile,
/// mutating classification, and sandbox policy.
pub struct PolicyEngine {
    mode: RwLock<AgentMode>,
    base_profile: PermissionProfile,
    approval_cache: RwLock<HashSet<String>>,
    /// Dynamic per-session permission grants.
    dynamic_grants: RwLock<HashSet<String>>,
    /// Per-thread permission overrides (thread_id → granted permissions).
    thread_grants: RwLock<HashMap<String, HashSet<String>>>,
}

impl PolicyEngine {
    /// Create a new policy engine for the given mode.
    pub fn new(mode: AgentMode, base_profile: PermissionProfile) -> Self {
        Self {
            mode: RwLock::new(mode),
            base_profile,
            approval_cache: RwLock::new(HashSet::new()),
            dynamic_grants: RwLock::new(HashSet::new()),
            thread_grants: RwLock::new(HashMap::new()),
        }
    }

    /// Get the current agent mode.
    pub fn mode(&self) -> AgentMode {
        *self.mode.read().unwrap()
    }

    /// Update the agent mode at runtime.
    pub fn set_mode(&self, mode: AgentMode) {
        *self.mode.write().unwrap() = mode;
    }

    /// Evaluate whether a tool call needs approval.
    pub fn evaluate(
        &self,
        tool_name: &str,
        is_mutating: bool,
        permission: Option<&PermissionType>,
    ) -> PolicyDecision {
        let mode = self.mode();

        // Plan mode: never approve any tool execution.
        if mode == AgentMode::Plan {
            return PolicyDecision {
                approval: ApprovalRequirement::Forbidden {
                    reason: "plan mode does not allow tool execution".into(),
                },
                sandbox_required: false,
                network_policy: NetworkPolicy::DenyAll,
            };
        }

        // Trusted mode: auto-approve everything except when sandbox is required.
        if mode == AgentMode::Trusted {
            return PolicyDecision {
                approval: ApprovalRequirement::Skip,
                sandbox_required: false,
                network_policy: NetworkPolicy::AllowAll,
            };
        }

        // Agent mode: mutating tools need approval. Read-only tools skip.
        if !is_mutating {
            return PolicyDecision {
                approval: ApprovalRequirement::Skip,
                sandbox_required: false,
                network_policy: NetworkPolicy::default(),
            };
        }

        // Check explicit permissions.
        if let Some(perm) = permission
            && !self.base_profile.is_allowed(perm) {
                return PolicyDecision {
                    approval: ApprovalRequirement::Forbidden {
                        reason: format!("permission '{perm:?}' is denied"),
                    },
                    sandbox_required: false,
                    network_policy: NetworkPolicy::DenyAll,
                };
            }

        // Check approval cache for session-remembered decisions.
        let cache_key = format!("{tool_name}:mutating");
        {
            let cache = self.approval_cache.read().unwrap();
            if cache.contains(&cache_key) {
                return PolicyDecision {
                    approval: ApprovalRequirement::Skip,
                    sandbox_required: false,
                    network_policy: NetworkPolicy::default(),
                };
            }
        }

        // Requires approval.
        let approval_id = format!("approval-{}", uuid::Uuid::new_v4().simple());
        PolicyDecision {
            approval: ApprovalRequirement::NeedsApproval {
                reason: format!("mutating tool '{tool_name}' requires approval"),
                approval_id,
            },
            sandbox_required: false,
            network_policy: NetworkPolicy::default(),
        }
    }

    /// Cache an approval for the rest of the session.
    pub fn remember_approval(&self, tool_name: &str) {
        let key = format!("{tool_name}:mutating");
        self.approval_cache.write().unwrap().insert(key);
    }

    /// Clear the approval cache.
    pub fn clear_approval_cache(&self) {
        self.approval_cache.write().unwrap().clear();
    }

    /// Grant a dynamic permission for the current session.
    pub fn grant_permission(&self, tool_pattern: &str) {
        self.dynamic_grants.write().unwrap().insert(tool_pattern.to_string());
    }

    /// Revoke a dynamic permission.
    pub fn revoke_permission(&self, tool_pattern: &str) {
        self.dynamic_grants.write().unwrap().remove(tool_pattern);
    }

    /// Grant a per-thread permission override.
    pub fn grant_thread_permission(&self, thread_id: &str, tool_pattern: &str) {
        self.thread_grants.write().unwrap()
            .entry(thread_id.to_string())
            .or_default()
            .insert(tool_pattern.to_string());
    }

    /// Check if a tool is dynamically granted.
    pub fn is_dynamically_granted(&self, tool_name: &str, thread_id: Option<&str>) -> bool {
        if self.dynamic_grants.read().unwrap().contains(tool_name) {
            return true;
        }
        if let Some(tid) = thread_id
            && let Some(grants) = self.thread_grants.read().unwrap().get(tid)
            && grants.contains(tool_name)
        {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_mode_rejects_all() {
        let engine = PolicyEngine::new(AgentMode::Plan, PermissionProfile::default());
        let decision = engine.evaluate("read_file", false, None);
        assert!(matches!(
            decision.approval,
            ApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn test_trusted_mode_approves_all() {
        let engine = PolicyEngine::new(AgentMode::Trusted, PermissionProfile::default());
        let decision = engine.evaluate("shell_exec", true, None);
        assert_eq!(decision.approval, ApprovalRequirement::Skip);
    }

    #[test]
    fn test_agent_mode_read_only_skips() {
        let engine = PolicyEngine::new(AgentMode::Agent, PermissionProfile::default());
        let decision = engine.evaluate("read_file", false, None);
        assert_eq!(decision.approval, ApprovalRequirement::Skip);
    }

    #[test]
    fn test_agent_mode_mutating_needs_approval() {
        let engine = PolicyEngine::new(AgentMode::Agent, PermissionProfile::default());
        let decision = engine.evaluate("write_file", true, None);
        assert!(matches!(
            decision.approval,
            ApprovalRequirement::NeedsApproval { .. }
        ));
    }

    #[test]
    fn test_agent_mode_denied_permission() {
        let profile = PermissionProfile::new().deny(PermissionType::ShellExec);
        let engine = PolicyEngine::new(AgentMode::Agent, profile);
        let decision = engine.evaluate("shell_exec", true, Some(&PermissionType::ShellExec));
        assert!(matches!(
            decision.approval,
            ApprovalRequirement::Forbidden { .. }
        ));
    }

    #[test]
    fn test_remember_approval_cache() {
        let engine = PolicyEngine::new(AgentMode::Agent, PermissionProfile::default());
        let decision1 = engine.evaluate("write_file", true, None);
        assert!(matches!(decision1.approval, ApprovalRequirement::NeedsApproval { .. }));

        engine.remember_approval("write_file");
        let decision2 = engine.evaluate("write_file", true, None);
        assert_eq!(decision2.approval, ApprovalRequirement::Skip);
    }

    #[test]
    fn test_permission_profile_default_allow() {
        let profile = PermissionProfile::new();
        assert!(profile.is_allowed(&PermissionType::FilesystemRead));
    }

    #[test]
    fn test_permission_profile_default_deny() {
        let profile = PermissionProfile::new().with_default_deny();
        assert!(!profile.is_allowed(&PermissionType::Network));
    }
}
