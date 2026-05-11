//! Integration test: Sub-agent lifecycle.
//! PRD §11.2: "Parent spawns child sub-agent, child completes."

use std::sync::Arc;

use deepomni_agent::{TurnConfig, TurnRunner};
use deepomni_journal::InMemoryJournal;
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::tool::ToolOutput;
use deepomni_test_support::{
    mock_text_response, MockModelProvider,
};
use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};

struct TestReadTool;
#[async_trait::async_trait]
impl ToolHandler for TestReadTool {
    fn name(&self) -> &str { "read_file" }
    async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Function { body: Some(serde_json::json!({"ok": true})), success: true })
    }
}

#[tokio::test]
async fn test_subagent_spawn_and_complete() {
    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestReadTool)).await;
        r
    });
    let policy = Arc::new(PolicyEngine::new(AgentMode::Trusted, PermissionProfile::new()));
    let journal = Arc::new(InMemoryJournal::new());
    let runner = Arc::new(TurnRunner::new(registry.clone(), policy.clone(), journal));

    let provider = Arc::new(MockModelProvider::new(vec![
        mock_text_response("sub-agent result: found 3 issues"),
    ]));

    let parent_thread_id = ThreadId::from_string("parent-thread");
    let parent_turn_id = TurnId::from_string("parent-turn");

    let result = runner.spawn_subagent(
        parent_thread_id,
        parent_turn_id,
        "Review this file".into(),
        provider,
        TurnConfig {
            model: "mock-model".into(),
            max_output_tokens: Some(1000),
            token_budget: 10_000,
            turn_timeout: None,
            agent_mode: AgentMode::Agent,
            system_prompt: None,
            workspace: "/tmp/test".into(),
        },
    ).await.expect("sub-agent should complete");

    assert_eq!(result.answer, "sub-agent result: found 3 issues");
    assert!(!result.subagent_id.as_str().is_empty());
}

#[tokio::test]
async fn test_subagent_failure() {
    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestReadTool)).await;
        r
    });
    let policy = Arc::new(PolicyEngine::new(AgentMode::Trusted, PermissionProfile::new()));
    let journal = Arc::new(InMemoryJournal::new());
    let runner = Arc::new(TurnRunner::new(registry, policy, journal));

    // Mock that returns nothing usable — stream ends immediately.
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_text_response(""),
    ]));

    let result = runner.spawn_subagent(
        ThreadId::from_string("parent-thread-fail"),
        TurnId::from_string("parent-turn-fail"),
        "Invalid task".into(),
        provider,
        TurnConfig {
            model: "mock-model".into(),
            max_output_tokens: Some(100),
            token_budget: 1_000,
            turn_timeout: None,
            agent_mode: AgentMode::Agent,
            system_prompt: None,
            workspace: "/tmp/test".into(),
        },
    ).await;

    // The sub-agent should complete even with empty output.
    assert!(result.is_ok());
}
