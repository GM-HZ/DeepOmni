//! Parallel/serial tool execution runtime. Plan 2 Task 1 + Task 5.
//!
//! Non-mutating tools that support parallel execution run concurrently
//! under a read lock. Mutating or exclusive tools run serially under
//! a write lock, preventing conflicts.
//!
//! Sandbox escalation: tools with `escalate_on_deny` that fail under sandbox
//! are retried unsandboxed once. Plan 2 Task 5.

use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::task::JoinSet;

use deepomni_protocol::id::ToolCallId;
use deepomni_protocol::tool::ToolOutput;
use deepomni_sandbox::SandboxAttempt;

use super::{ToolError, ToolInvocation, ToolRegistry};

/// A tool call ready for execution, with mutability and concurrency metadata.
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub invocation: ToolInvocation,
    pub is_mutating: bool,
    pub supports_parallel: bool,
    /// Sandbox strategy for this call. Disabled by default.
    pub sandbox_attempt: SandboxAttempt,
}

/// Result of executing a single tool call within a batch.
#[derive(Debug)]
pub struct ToolCallResult {
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub output: Result<ToolOutput, ToolError>,
}

/// Executes tool call batches with read/write lock concurrency.
#[derive(Clone)]
pub struct ToolCallRuntime {
    registry: Arc<ToolRegistry>,
    parallel_lock: Arc<RwLock<()>>,
}

impl ToolCallRuntime {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            registry,
            parallel_lock: Arc::new(RwLock::new(())),
        }
    }

    /// Execute a batch of pending tool calls respecting input order.
    /// Consecutive non-mutating + parallel-safe calls run concurrently;
    /// exclusive/mutating calls flush the preceding parallel batch first,
    /// then execute serially under a write lock.
    pub async fn execute_batch(&self, calls: Vec<PendingToolCall>) -> Vec<ToolCallResult> {
        let total = calls.len();
        let mut ordered: Vec<Option<ToolCallResult>> =
            std::iter::repeat_with(|| None).take(total).collect();
        let mut pending_parallel: Vec<(usize, PendingToolCall)> = Vec::new();
        let mut parallel_join = JoinSet::new();

        for (index, call) in calls.into_iter().enumerate() {
            if !call.is_mutating && call.supports_parallel {
                pending_parallel.push((index, call));
            } else {
                // Flush accumulated parallel batch before this exclusive call.
                if !pending_parallel.is_empty() {
                    for (pi, pc) in pending_parallel.drain(..) {
                        let runtime = self.clone();
                        parallel_join.spawn(async move {
                            let _guard = runtime.parallel_lock.read().await;
                            (pi, runtime.execute_single(pc).await)
                        });
                    }
                    while let Some(joined) = parallel_join.join_next().await {
                        if let Ok((pi, result)) = joined {
                            ordered[pi] = Some(result);
                        }
                    }
                }
                // Execute exclusive call serially.
                let _guard = self.parallel_lock.write().await;
                ordered[index] = Some(self.execute_single(call).await);
            }
        }

        // Flush any remaining parallel batch.
        if !pending_parallel.is_empty() {
            for (pi, pc) in pending_parallel.drain(..) {
                let runtime = self.clone();
                parallel_join.spawn(async move {
                    let _guard = runtime.parallel_lock.read().await;
                    (pi, runtime.execute_single(pc).await)
                });
            }
            while let Some(joined) = parallel_join.join_next().await {
                if let Ok((pi, result)) = joined {
                    ordered[pi] = Some(result);
                }
            }
        }

        ordered.into_iter().flatten().collect()
    }

    async fn execute_single(&self, call: PendingToolCall) -> ToolCallResult {
        let call_id = call.invocation.call_id.clone();
        let tool_name = call.invocation.tool_name.clone();

        // First attempt: execute with sandbox if required.
        let output = self.registry.dispatch(call.invocation.clone()).await;

        // Sandbox escalation: if the first attempt failed and escalation is
        // allowed, retry without sandbox. V1 sandbox is a pass-through so
        // this path is exercised only when platform sandbox enforcement
        // returns a denial error.
        let final_output = match (&output, call.sandbox_attempt) {
            (
                Err(_),
                SandboxAttempt::Required {
                    escalate_on_deny: true,
                },
            )
            | (Err(_), SandboxAttempt::PreferredWithEscalation) => {
                // Retry with sandbox disabled.
                let mut retry_invocation = call.invocation;
                retry_invocation.allow_mutating = true; // already evaluated
                self.registry.dispatch(retry_invocation).await
            }
            _ => output,
        };

        ToolCallResult {
            call_id,
            tool_name,
            output: final_output,
        }
    }
}
