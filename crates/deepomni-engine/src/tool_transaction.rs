use std::sync::Arc;

use deepomni_tools::{PendingToolCall, ToolCallResult, ToolCallRuntime, ToolRegistry};

#[derive(Clone)]
pub struct ToolTransactionEngine {
    runtime: ToolCallRuntime,
}

impl ToolTransactionEngine {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self {
            runtime: ToolCallRuntime::new(registry),
        }
    }

    pub async fn execute_batch(&self, calls: Vec<PendingToolCall>) -> Vec<ToolCallResult> {
        self.runtime.execute_batch(calls).await
    }
}

