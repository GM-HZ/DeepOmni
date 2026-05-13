//! # DeepOmni Hooks
//!
//! Lifecycle hooks that fire at key points during session and turn execution.
//! Hooks are non-blocking, isolated from each other (one hook failure does
//! not affect others or the agent loop), and can inject additional context
//! or emit side effects.
//!
//! PRD §7.17. Based on DeepSeek-TUI HookDispatcher patterns.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use deepomni_protocol::id::{ThreadId, ToolCallId, TurnId};

/// Hook points in the agent lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    SessionStart,
    UserPromptSubmitted,
    BeforeToolCall,
    PermissionRequested,
    AfterToolCall,
    TurnCompleted,
    SessionStop,
}

/// Context passed to a hook on each invocation.
#[derive(Debug, Clone)]
pub struct HookContext {
    pub hook_point: HookPoint,
    pub thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<ToolCallId>,
    /// Arbitrary data the hook may use.
    pub data: Option<String>,
    /// Whether the tool call was approved/rejected.
    pub approved: Option<bool>,
}

/// Result of a hook execution.
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// Hook completed without side effects.
    Ok,
    /// Hook wants to inject additional context into the prompt.
    InjectContext { content: String },
    /// Hook wants to emit a warning message.
    Warning { message: String },
}

/// A registered hook with its metadata.
#[derive(Debug, Clone)]
pub struct Hook {
    pub id: String,
    pub name: String,
    pub hook_points: Vec<HookPoint>,
    pub plugin_id: Option<String>,
    /// Optional timeout in milliseconds. Default: 5000.
    pub timeout_ms: u64,
}

/// Trait implemented by hook handlers.
#[async_trait]
pub trait HookHandler: Send + Sync {
    /// Return the hook metadata.
    fn metadata(&self) -> &Hook;

    /// Execute the hook.
    async fn run(&self, ctx: HookContext) -> Result<HookOutcome, String>;
}

/// Dispatches hooks across all registered handlers.
pub struct HookDispatcher {
    handlers: RwLock<HashMap<String, Arc<dyn HookHandler>>>,
    /// Hooks are grouped by hook point for efficient dispatch.
    by_point: RwLock<HashMap<HookPoint, Vec<String>>>,
}

impl HookDispatcher {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
            by_point: RwLock::new(HashMap::new()),
        }
    }

    /// Register a hook handler.
    pub async fn register(&self, handler: Arc<dyn HookHandler>) {
        let meta = handler.metadata();
        let hook_id = meta.id.clone();

        {
            let mut by_point = self.by_point.write().await;
            for point in &meta.hook_points {
                by_point.entry(*point).or_default().push(hook_id.clone());
            }
        }

        self.handlers.write().await.insert(hook_id, handler);
    }

    /// Remove all hooks registered by a plugin.
    pub async fn remove_plugin_hooks(&self, plugin_id: &str) -> usize {
        let handlers = self.handlers.read().await;
        let to_remove: Vec<String> = handlers
            .values()
            .filter(|h| h.metadata().plugin_id.as_deref() == Some(plugin_id))
            .map(|h| h.metadata().id.clone())
            .collect();

        drop(handlers);
        let mut removed = 0;
        let mut handlers = self.handlers.write().await;
        let mut by_point = self.by_point.write().await;

        for id in &to_remove {
            if handlers.remove(id).is_some() {
                removed += 1;
                // Clean up by_point index.
                for hooks in by_point.values_mut() {
                    hooks.retain(|h| h != id);
                }
            }
        }

        removed
    }

    /// Dispatch to all hooks registered for the given hook point.
    ///
    /// Each hook runs with a timeout. Failures in one hook do not
    /// affect other hooks or the agent loop.
    pub async fn dispatch(&self, point: HookPoint, ctx: HookContext) -> Vec<HookOutcome> {
        let hook_ids = {
            let by_point = self.by_point.read().await;
            by_point.get(&point).cloned().unwrap_or_default()
        };

        if hook_ids.is_empty() {
            return vec![];
        }

        let handlers = self.handlers.read().await;
        let mut outcomes = Vec::new();

        for hook_id in &hook_ids {
            let handler = match handlers.get(hook_id) {
                Some(h) => h.clone(),
                None => continue,
            };
            let timeout_ms = handler.metadata().timeout_ms.max(1000);

            match tokio::time::timeout(Duration::from_millis(timeout_ms), handler.run(ctx.clone()))
                .await
            {
                Ok(Ok(outcome)) => outcomes.push(outcome),
                Ok(Err(e)) => {
                    warn!(
                        hook_id = %hook_id,
                        error = %e,
                        "hook execution failed (non-fatal)"
                    );
                }
                Err(_) => {
                    warn!(
                        hook_id = %hook_id,
                        timeout_ms = timeout_ms,
                        "hook timed out (non-fatal)"
                    );
                }
            }
        }

        outcomes
    }

    /// Convenience: fire hooks and collect any context injections.
    pub async fn dispatch_and_collect_context(&self, point: HookPoint, ctx: HookContext) -> String {
        let outcomes = self.dispatch(point, ctx).await;
        let mut injected = String::new();
        for outcome in outcomes {
            if let HookOutcome::InjectContext { content } = outcome {
                injected.push_str(&content);
                injected.push('\n');
            }
        }
        injected
    }

    pub async fn hook_count(&self) -> usize {
        self.handlers.read().await.len()
    }
}

impl Default for HookDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in hook: stdout logger ──

/// A simple hook handler that logs hook invocations (for debugging).
pub struct LoggingHook {
    meta: Hook,
}

impl Default for LoggingHook {
    fn default() -> Self {
        Self::new()
    }
}

impl LoggingHook {
    pub fn new() -> Self {
        Self {
            meta: Hook {
                id: "deepomni.logging".into(),
                name: "Logging Hook".into(),
                hook_points: vec![
                    HookPoint::SessionStart,
                    HookPoint::UserPromptSubmitted,
                    HookPoint::BeforeToolCall,
                    HookPoint::AfterToolCall,
                    HookPoint::TurnCompleted,
                    HookPoint::SessionStop,
                ],
                plugin_id: None,
                timeout_ms: 2000,
            },
        }
    }
}

#[async_trait]
impl HookHandler for LoggingHook {
    fn metadata(&self) -> &Hook {
        &self.meta
    }

    async fn run(&self, ctx: HookContext) -> Result<HookOutcome, String> {
        tracing::info!(
            hook_point = ?ctx.hook_point,
            thread_id = ?ctx.thread_id,
            tool_name = ?ctx.tool_name,
            "hook fired"
        );
        Ok(HookOutcome::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook {
        meta: Hook,
    }

    impl TestHook {
        fn new(id: &str) -> Self {
            Self {
                meta: Hook {
                    id: id.to_string(),
                    name: "Test Hook".into(),
                    hook_points: vec![HookPoint::BeforeToolCall],
                    plugin_id: None,
                    timeout_ms: 5000,
                },
            }
        }
    }

    #[async_trait]
    impl HookHandler for TestHook {
        fn metadata(&self) -> &Hook {
            &self.meta
        }

        async fn run(&self, _ctx: HookContext) -> Result<HookOutcome, String> {
            Ok(HookOutcome::InjectContext {
                content: "test context".into(),
            })
        }
    }

    #[tokio::test]
    async fn test_dispatch_hook() {
        let dispatcher = HookDispatcher::new();
        dispatcher.register(Arc::new(TestHook::new("test.1"))).await;

        let outcomes = dispatcher
            .dispatch(
                HookPoint::BeforeToolCall,
                HookContext {
                    hook_point: HookPoint::BeforeToolCall,
                    thread_id: None,
                    turn_id: None,
                    tool_name: Some("read_file".into()),
                    tool_call_id: None,
                    data: None,
                    approved: None,
                },
            )
            .await;

        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], HookOutcome::InjectContext { .. }));
    }

    #[tokio::test]
    async fn test_dispatch_no_matching_hooks() {
        let dispatcher = HookDispatcher::new();
        dispatcher.register(Arc::new(TestHook::new("test.2"))).await;

        let outcomes = dispatcher
            .dispatch(
                HookPoint::SessionStop,
                HookContext {
                    hook_point: HookPoint::SessionStop,
                    thread_id: None,
                    turn_id: None,
                    tool_name: None,
                    tool_call_id: None,
                    data: None,
                    approved: None,
                },
            )
            .await;

        assert!(outcomes.is_empty());
    }

    #[tokio::test]
    async fn test_remove_plugin_hooks() {
        struct PluginHook {
            meta: Hook,
        }
        #[async_trait]
        impl HookHandler for PluginHook {
            fn metadata(&self) -> &Hook {
                &self.meta
            }
            async fn run(&self, _ctx: HookContext) -> Result<HookOutcome, String> {
                Ok(HookOutcome::Ok)
            }
        }

        let dispatcher = HookDispatcher::new();
        dispatcher
            .register(Arc::new(PluginHook {
                meta: Hook {
                    id: "myplugin.hook".into(),
                    name: "Plugin Hook".into(),
                    hook_points: vec![HookPoint::TurnCompleted],
                    plugin_id: Some("myplugin".into()),
                    timeout_ms: 5000,
                },
            }))
            .await;
        assert_eq!(dispatcher.hook_count().await, 1);

        let removed = dispatcher.remove_plugin_hooks("myplugin").await;
        assert_eq!(removed, 1);
        assert_eq!(dispatcher.hook_count().await, 0);
    }
}
