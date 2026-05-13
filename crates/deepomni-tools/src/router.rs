//! Tool router: translates raw model tool-call output into normalized
//! PendingToolCall metadata. Plan 2 Task 2.

use std::time::Duration;

use deepomni_protocol::id::ToolCallId;
use deepomni_protocol::tool::{ToolPayload, ToolSpec};
use deepomni_sandbox::SandboxAttempt;

use super::{PendingToolCall, SandboxPreference, ToolInvocation, ToolRegistry};

/// Translates raw model output into normalized execution metadata.
/// Reads handler policy to determine mutability and concurrency safety.
pub struct ToolRouter {
    model_visible_specs: Vec<ToolSpec>,
}

impl ToolRouter {
    pub fn new(model_visible_specs: Vec<ToolSpec>) -> Self {
        Self {
            model_visible_specs,
        }
    }

    pub fn model_visible_specs(&self) -> &[ToolSpec] {
        &self.model_visible_specs
    }

    /// Build a PendingToolCall from a function call coming from the model.
    /// Unknown tools default to mutating=true for safety.
    pub fn build_pending_call(
        &self,
        registry: &ToolRegistry,
        call_id: ToolCallId,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> PendingToolCall {
        let handler = registry.get(tool_name);
        let is_mutating = handler.map(|h| h.is_mutating()).unwrap_or(true);
        let supports_parallel = handler.map(|h| h.supports_parallel()).unwrap_or(false);

        let sandbox_attempt = match handler.map(|h| h.sandbox_preference()) {
            Some(SandboxPreference::Required) => SandboxAttempt::Required {
                escalate_on_deny: false,
            },
            Some(SandboxPreference::Sandboxed) => SandboxAttempt::Required {
                escalate_on_deny: true,
            },
            Some(SandboxPreference::Unsandboxed) | None => SandboxAttempt::Disabled,
        };

        PendingToolCall {
            invocation: ToolInvocation {
                call_id,
                tool_name: tool_name.to_string(),
                payload: ToolPayload::Function {
                    arguments: serde_json::to_string(&arguments).unwrap_or_default(),
                },
                timeout: Some(Duration::from_secs(60)),
                allow_mutating: is_mutating,
                workspace: None,
            },
            is_mutating,
            supports_parallel,
            sandbox_attempt,
        }
    }
}
