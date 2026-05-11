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
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::task::JoinSet;

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

    /// Evaluate with a session-local approval cache. Cached allow/deny
    /// decisions short-circuit the normal approval prompt path.
    pub fn evaluate_with_cache<K: Serialize>(
        handler: &dyn ToolHandler,
        agent_mode: &deepomni_policy::AgentMode,
        approval_store: &ApprovalStore,
        approval_key: &K,
    ) -> OrchestratorDecision {
        match approval_store.get(approval_key) {
            Some(ApprovalDecision::Approved) => {
                let sandbox_preference = handler.sandbox_preference();
                OrchestratorDecision::Proceed {
                    sandbox_required: matches!(sandbox_preference, SandboxPreference::Required),
                    escalate_on_deny: matches!(sandbox_preference, SandboxPreference::Sandboxed),
                }
            }
            Some(ApprovalDecision::Rejected) => OrchestratorDecision::Forbidden {
                reason: "tool call rejected by cached approval decision".into(),
            },
            None => Self::evaluate(handler, agent_mode),
        }
    }
}

// ── Approval session cache ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Rejected,
}

#[derive(Debug, Default)]
pub struct ApprovalStore {
    cache: std::sync::Mutex<HashMap<String, ApprovalDecision>>,
}

impl ApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get<K: Serialize>(&self, key: &K) -> Option<ApprovalDecision> {
        let key = approval_cache_key(key).ok()?;
        self.cache.lock().ok()?.get(&key).copied()
    }

    pub fn put<K: Serialize>(&self, key: K, decision: ApprovalDecision) {
        if let Ok(key) = approval_cache_key(&key)
            && let Ok(mut cache) = self.cache.lock()
        {
            cache.insert(key, decision);
        }
    }

    pub async fn with_cached_approval<K, F, Fut>(&self, keys: Vec<K>, fetch: F) -> ApprovalDecision
    where
        K: Serialize,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ApprovalDecision>,
    {
        for key in &keys {
            if let Some(decision) = self.get(key) {
                return decision;
            }
        }

        let decision = fetch().await;
        for key in keys {
            self.put(key, decision);
        }
        decision
    }

    pub fn len(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn approval_cache_key<K: Serialize>(key: &K) -> Result<String, serde_json::Error> {
    serde_json::to_string(key)
}

// ── Parallel tool execution ──

#[derive(Clone)]
pub struct ToolCallRuntime {
    registry: Arc<ToolRegistry>,
    parallel_lock: Arc<RwLock<()>>,
}

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub invocation: ToolInvocation,
    pub is_mutating: bool,
    pub supports_parallel: bool,
}

#[derive(Debug)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub output: Result<ToolOutput, ToolError>,
}

impl ToolCallRuntime {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            parallel_lock: Arc::new(RwLock::new(())),
        }
    }

    pub async fn execute_batch(&self, calls: Vec<PendingToolCall>) -> Vec<ToolCallResult> {
        let total = calls.len();
        let mut ordered: Vec<Option<ToolCallResult>> =
            std::iter::repeat_with(|| None).take(total).collect();
        let mut parallel = JoinSet::new();
        let mut exclusive = Vec::new();

        for (index, call) in calls.into_iter().enumerate() {
            if !call.is_mutating && call.supports_parallel {
                let runtime = self.clone();
                parallel.spawn(async move {
                    let _guard = runtime.parallel_lock.read().await;
                    (index, runtime.execute_single(call).await)
                });
            } else {
                exclusive.push((index, call));
            }
        }

        while let Some(joined) = parallel.join_next().await {
            if let Ok((index, result)) = joined {
                ordered[index] = Some(result);
            }
        }

        for (index, call) in exclusive {
            let _guard = self.parallel_lock.write().await;
            ordered[index] = Some(self.execute_single(call).await);
        }

        ordered.into_iter().flatten().collect()
    }

    async fn execute_single(&self, call: PendingToolCall) -> ToolCallResult {
        let call_id = call.invocation.call_id.clone();
        let tool_name = call.invocation.tool_name.clone();
        let output = self.registry.dispatch(call.invocation).await;
        ToolCallResult {
            call_id,
            tool_name,
            output,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    #[tokio::test]
    async fn approval_store_caches_decision_for_all_keys() {
        let store = ApprovalStore::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_fetch = calls.clone();

        let decision = store
            .with_cached_approval(
                vec![
                    serde_json::json!({"tool": "write", "path": "a"}),
                    serde_json::json!({"tool": "write", "path": "b"}),
                ],
                move || async move {
                    calls_for_fetch.fetch_add(1, Ordering::SeqCst);
                    ApprovalDecision::Approved
                },
            )
            .await;

        assert_eq!(decision, ApprovalDecision::Approved);
        assert_eq!(store.len(), 2);

        let cached = store
            .with_cached_approval(
                vec![serde_json::json!({"tool": "write", "path": "b"})],
                || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    ApprovalDecision::Rejected
                },
            )
            .await;

        assert_eq!(cached, ApprovalDecision::Approved);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn orchestrator_uses_cached_rejection_before_prompting() {
        let store = ApprovalStore::new();
        let key = serde_json::json!({"tool": "fake_read"});
        store.put(key.clone(), ApprovalDecision::Rejected);

        let decision = ToolOrchestrator::evaluate_with_cache(
            &FakeReadTool,
            &deepomni_policy::AgentMode::Agent,
            &store,
            &key,
        );

        assert!(matches!(decision, OrchestratorDecision::Forbidden { .. }));
    }

    struct ConcurrentTool {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolHandler for ConcurrentTool {
        fn name(&self) -> &str { "concurrent" }
        fn supports_parallel(&self) -> bool { true }

        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput::Function { body: None, success: true })
        }
    }

    #[tokio::test]
    async fn tool_call_runtime_runs_parallel_safe_tools_concurrently() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(ConcurrentTool {
                active,
                max_active: max_active.clone(),
            }))
            .await;
        let runtime = ToolCallRuntime::new(Arc::new(registry));

        let calls = (0..2)
            .map(|_| PendingToolCall {
                invocation: ToolInvocation {
                    call_id: ToolCallId::new(),
                    tool_name: "concurrent".into(),
                    payload: ToolPayload::Function { arguments: "{}".into() },
                    timeout: None,
                    allow_mutating: false,
                    workspace: None,
                },
                is_mutating: false,
                supports_parallel: true,
            })
            .collect();

        let results = runtime.execute_batch(calls).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.output.is_ok()));
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
    }
}
