//! # DeepOmni Tools
//!
//! Tool traits, registry, invocation, output model, and MCP/native unified
//! tool wrapper. Based on Codex `ToolHandler` + DeepSeek-TUI `ToolRegistry`.
//!
//! PRD §7.7

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::RwLock;

use deepomni_protocol::id::ToolCallId;
use deepomni_protocol::tool::{ToolOutput, ToolPayload, ToolSpec};

// ── Tool capabilities ──

/// Classification of tool mutability and resource needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCapability {
    ReadOnly,
    WritesFiles,
    ExecutesCode,
    Network,
    Sandboxable,
    RequiresApproval,
}

/// Approval default for a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApprovalDefault {
    #[default]
    Auto,
    Always,
    Never,
}

// ── Tool policy trait ──

/// Sandbox preference for a tool. Used by ToolOrchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPreference {
    /// Tool prefers sandboxed execution.
    Sandboxed,
    /// Tool prefers unsandboxed execution.
    Unsandboxed,
    /// Sandbox is required (hard deny without it).
    Required,
}

/// Policy controls every tool declares. The ToolOrchestrator reads these
/// to run the approve → sandbox → execute → retry pipeline uniformly,
/// without inline policy checks in the agent loop.
pub trait ToolPolicy {
    /// Default approval behavior for this tool.
    fn approval_default(&self) -> ApprovalDefault {
        ApprovalDefault::Auto
    }

    /// Sandbox preference for this tool.
    fn sandbox_preference(&self) -> SandboxPreference {
        SandboxPreference::Unsandboxed
    }

    /// Whether the tool mutates state (triggers approval in agent mode).
    fn is_mutating(&self) -> bool {
        false
    }

    /// Capabilities this tool uses (for permission checking).
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }
}

// ── Tool handler trait ──

/// The handler trait for all tools — native, MCP, or plugin.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Unique tool name.
    fn name(&self) -> &str;

    /// Tool schema for model visibility.
    fn spec(&self) -> Option<ToolSpec> {
        None
    }

    /// Whether this tool mutates state.
    fn is_mutating(&self) -> bool {
        false
    }

    /// Whether this tool can run in parallel.
    fn supports_parallel(&self) -> bool {
        false
    }

    /// Default approval behavior.
    fn approval_default(&self) -> ApprovalDefault {
        ApprovalDefault::Auto
    }

    /// Capabilities this tool uses.
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    /// Sandbox preference for this tool.
    fn sandbox_preference(&self) -> SandboxPreference {
        SandboxPreference::Unsandboxed
    }

    /// Execute the tool and return an output.
    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, ToolError>;
}

// ── Tool invocation ──

/// A complete tool invocation ready for execution.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub payload: ToolPayload,
    /// Timeout for execution.
    pub timeout: Option<Duration>,
    /// Whether the tool call is allowed to mutate state.
    pub allow_mutating: bool,
    /// Per-invocation workspace root (set from thread cwd).
    pub workspace: Option<String>,
}

// ── Tool error ──

#[derive(Debug, Clone)]
pub enum ToolError {
    InvalidInput { message: String },
    MissingField { field: String },
    PathEscape { path: String },
    ExecutionFailed { message: String },
    Timeout { seconds: u64 },
    NotAvailable { message: String },
    PermissionDenied { message: String },
    MutatingToolRejected,
    Cancelled,
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::InvalidInput { message } => write!(f, "invalid input: {message}"),
            ToolError::MissingField { field } => write!(f, "missing required field: {field}"),
            ToolError::PathEscape { path } => write!(f, "path escapes workspace: {path}"),
            ToolError::ExecutionFailed { message } => write!(f, "execution failed: {message}"),
            ToolError::Timeout { seconds } => write!(f, "timed out after {seconds}s"),
            ToolError::NotAvailable { message } => write!(f, "not available: {message}"),
            ToolError::PermissionDenied { message } => write!(f, "permission denied: {message}"),
            ToolError::MutatingToolRejected => write!(f, "mutating tool rejected by policy"),
            ToolError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for ToolError {}

// ── Tool registry ──

/// Registry of all registered tool handlers.
pub struct ToolRegistry {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    spec_cache: RwLock<Vec<ToolSpec>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            spec_cache: RwLock::new(Vec::new()),
        }
    }

    /// Register a tool handler.
    pub async fn register(&mut self, handler: Arc<dyn ToolHandler>) {
        if let Some(spec) = handler.spec() {
            self.spec_cache.write().await.push(spec);
        }
        self.handlers.insert(handler.name().to_string(), handler);
    }

    /// List all tool specs for model consumption.
    pub async fn list_specs(&self) -> Vec<ToolSpec> {
        self.spec_cache.read().await.clone()
    }

    /// Look up a handler by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ToolHandler>> {
        self.handlers.get(name)
    }

    /// Dispatch a tool invocation.
    pub async fn dispatch(
        &self,
        invocation: ToolInvocation,
    ) -> Result<ToolOutput, ToolError> {
        let handler = self
            .get(&invocation.tool_name)
            .ok_or_else(|| ToolError::NotAvailable {
                message: format!("unknown tool: {}", invocation.tool_name),
            })?;

        if handler.is_mutating() && !invocation.allow_mutating {
            return Err(ToolError::MutatingToolRejected);
        }

        // Apply timeout if configured.
        if let Some(timeout) = invocation.timeout {
            match tokio::time::timeout(timeout, handler.handle(invocation)).await {
                Ok(result) => result,
                Err(_) => Err(ToolError::Timeout {
                    seconds: timeout.as_secs(),
                }),
            }
        } else {
            handler.handle(invocation).await
        }
    }

    /// Check if a tool name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tool orchestrator ──

/// Encapsulates the approve → sandbox → execute → retry pipeline.
/// Reads tool policy from the handler and coordinates the flow uniformly
/// for all tool types, removing inline checks from the agent loop.
pub struct ToolOrchestrator;

/// Result of orchestrator's pre-execution evaluation.
#[derive(Debug)]
pub enum OrchestratorDecision {
    /// Tool can execute immediately.
    Proceed {
        sandbox_required: bool,
        /// If true and sandbox execution fails, retry without sandbox.
        escalate_on_deny: bool,
    },
    /// Tool requires host approval before execution.
    NeedsApproval {
        reason: String,
        approval_id: String,
    },
    /// Tool is blocked by policy.
    Forbidden {
        reason: String,
    },
}

impl ToolOrchestrator {
    /// Evaluate whether a tool call can proceed, needs approval, or is forbidden.
    /// This reads the handler's policy declarations and applies agent mode rules.
    pub fn evaluate(
        handler: &dyn ToolHandler,
        agent_mode: &deepomni_policy::AgentMode,
    ) -> OrchestratorDecision {
        let is_mutating = handler.is_mutating();
        let approval_default = handler.approval_default();

        // Plan mode: never approve.
        if *agent_mode == deepomni_policy::AgentMode::Plan {
            return OrchestratorDecision::Forbidden {
                reason: "plan mode does not allow tool execution".into(),
            };
        }

        // Trusted mode: always proceed.
        if *agent_mode == deepomni_policy::AgentMode::Trusted {
            let escalate = matches!(handler.sandbox_preference(), SandboxPreference::Sandboxed);
            return OrchestratorDecision::Proceed {
                sandbox_required: matches!(
                    handler.sandbox_preference(),
                    SandboxPreference::Required
                ),
                escalate_on_deny: escalate,
            };
        }

        // Agent mode: read-only tools auto-proceed.
        if !is_mutating && approval_default != ApprovalDefault::Always {
            return OrchestratorDecision::Proceed {
                sandbox_required: false,
                escalate_on_deny: false,
            };
        }

        // Mutating tools or always-approve tools need approval.
        let approval_id = format!("approval-{}", uuid::Uuid::new_v4().simple());
        OrchestratorDecision::NeedsApproval {
            reason: format!(
                "tool '{}' requires approval (mutating={is_mutating})",
                handler.name()
            ),
            approval_id,
        }
    }
}

// ── JSON argument extraction helpers ──

/// Extract a required string field from JSON arguments.
pub fn required_str(args: &serde_json::Value, field: &str) -> Result<String, ToolError> {
    args.get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::MissingField {
            field: field.to_string(),
        })
}

/// Extract an optional string field.
pub fn optional_str(args: &serde_json::Value, field: &str) -> Option<String> {
    args.get(field).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Extract a required u64 field.
pub fn required_u64(args: &serde_json::Value, field: &str) -> Result<u64, ToolError> {
    args.get(field)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ToolError::MissingField {
            field: field.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    

    /// A fake tool for testing.
    struct FakeReadTool;

    #[async_trait]
    impl ToolHandler for FakeReadTool {
        fn name(&self) -> &str {
            "fake_read"
        }

        fn is_mutating(&self) -> bool {
            false
        }

        fn spec(&self) -> Option<ToolSpec> {
            Some(ToolSpec::Function(deepomni_protocol::tool::ToolSpecDetails {
                name: "fake_read".into(),
                description: "Fake read tool".into(),
                parameters: serde_json::json!({"type": "object"}),
            }))
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Function {
                body: Some(serde_json::json!({"result": "ok"})),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn test_registry_dispatch() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FakeReadTool)).await;

        let specs = registry.list_specs().await;
        assert_eq!(specs.len(), 1);

        let result = registry
            .dispatch(ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "fake_read".into(),
                payload: ToolPayload::Function {
                    arguments: "{}".into(),
                },
                timeout: None,
                allow_mutating: false,
                workspace: None,
            })
            .await
            .unwrap();

        match result {
            ToolOutput::Function { success, .. } => assert!(success),
            _ => panic!("wrong output type"),
        }
    }

    #[tokio::test]
    async fn test_mutating_rejected() {
        struct FakeWriteTool;
        #[async_trait]
        impl ToolHandler for FakeWriteTool {
            fn name(&self) -> &str { "fake_write" }
            fn is_mutating(&self) -> bool { true }
            async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::Function { body: None, success: true })
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FakeWriteTool)).await;

        let result = registry
            .dispatch(ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "fake_write".into(),
                payload: ToolPayload::Function { arguments: "{}".into() },
                timeout: None,
                allow_mutating: false,
                workspace: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::MutatingToolRejected)));
    }

    #[test]
    fn test_required_str() {
        let args = serde_json::json!({"path": "/tmp/test.txt"});
        assert_eq!(required_str(&args, "path").unwrap(), "/tmp/test.txt");
        assert!(required_str(&args, "missing").is_err());
    }

    #[test]
    fn test_required_u64() {
        let args = serde_json::json!({"count": 42});
        assert_eq!(required_u64(&args, "count").unwrap(), 42);
    }
}
