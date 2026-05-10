//! Integration test: Full turn with mock provider and built-in tool.
//! PRD §11.2: "Full turn with mock model and built-in tool."

use std::sync::Arc;

use deepomni_agent::{RunTurnRequest, TurnConfig, TurnResult, TurnRunner};
use deepomni_events::EventBus;
use deepomni_model_provider::ModelProvider;
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::tool::{ToolOutput, ToolPayload};
use deepomni_test_support::{
    mock_text_response, mock_tool_call_response, FakeToolHandler, MockModelProvider,
};
use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};

/// Simple read tool for integration testing.
struct TestReadTool;
#[async_trait::async_trait]
impl ToolHandler for TestReadTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn is_mutating(&self) -> bool {
        false
    }
    async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({"content": "hello world", "size": 11})),
            success: true,
        })
    }
}

#[tokio::test]
async fn test_full_turn_text_only() {
    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestReadTool)).await;
        r
    });
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Agent,
        PermissionProfile::new(),
    ));
    let events = Arc::new(EventBus::new());

    let runner = TurnRunner::new(registry, policy.clone(), events.clone());
    let provider = Arc::new(MockModelProvider::new(vec![mock_text_response(
        "The file contains hello world",
    )]));

    let thread_id = ThreadId::from_string("thread-full-turn");
    let turn_id = TurnId::from_string("turn-full-turn");

    let result = runner
        .run_turn(RunTurnRequest {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            user_input: "What does the file contain?".into(),
            config: TurnConfig {
                model: "mock-model".into(),
                max_output_tokens: Some(100),
                token_budget: 10_000,
                turn_timeout: None,
                agent_mode: AgentMode::Agent,
                system_prompt: None,
                workspace: "/tmp/test".into(),
            },
            provider,
            conversation_history: vec![],
            reasoning_to_replay: None,
            item_sink: None,
        })
        .await
        .expect("turn should complete");

    match result {
        TurnResult::Completed { answer, .. } => {
            assert!(
                answer.contains("hello world"),
                "answer should contain the mock response: {answer}"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn test_full_turn_with_tool_call() {
    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestReadTool)).await;
        r
    });
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Agent,
        PermissionProfile::new(),
    ));
    let events = Arc::new(EventBus::new());
    let runner = TurnRunner::new(registry, policy.clone(), events.clone());

    // Single mock response that includes a tool call for a non-mutating read tool.
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response(
            "read_file",
            serde_json::json!({"path": "src/main.rs"}),
        ),
    ]));

    let result = runner
        .run_turn(RunTurnRequest {
            thread_id: ThreadId::from_string("t1"),
            turn_id: TurnId::from_string("tu1"),
            user_input: "read src/main.rs".into(),
            config: TurnConfig {
                model: "mock-model".into(),
                max_output_tokens: Some(100),
                token_budget: 10_000,
                turn_timeout: None,
                agent_mode: AgentMode::Agent,
                system_prompt: None,
                workspace: "/tmp/test".into(),
            },
            provider,
            conversation_history: vec![],
            reasoning_to_replay: None,
            item_sink: None,
        })
        .await
        .expect("turn should complete");

    // Non-mutating tool in Agent mode should execute and complete.
    assert!(
        matches!(result, TurnResult::Completed { .. }),
        "expected Completed turn with tool executed, got {result:?}"
    );
}

#[tokio::test]
async fn test_mutating_tool_requires_approval() {
    // A mutating tool.
    struct TestWriteTool;
    #[async_trait::async_trait]
    impl ToolHandler for TestWriteTool {
        fn name(&self) -> &str {
            "write_file"
        }
        fn is_mutating(&self) -> bool {
            true
        }
        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Function {
                body: Some(serde_json::json!({"written": 100})),
                success: true,
            })
        }
    }

    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestWriteTool)).await;
        r
    });
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Agent,
        PermissionProfile::new(),
    ));
    let events = Arc::new(EventBus::new());

    let runner = TurnRunner::new(registry, policy.clone(), events.clone());
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response(
            "write_file",
            serde_json::json!({"path": "out.txt", "content": "data"}),
        ),
    ]));

    let result = runner
        .run_turn(RunTurnRequest {
            thread_id: ThreadId::from_string("thread-approval"),
            turn_id: TurnId::from_string("turn-approval"),
            user_input: "Write data to out.txt".into(),
            config: TurnConfig {
                model: "mock-model".into(),
                max_output_tokens: Some(100),
                token_budget: 10_000,
                turn_timeout: None,
                agent_mode: AgentMode::Agent,
                system_prompt: None,
                workspace: "/tmp/test".into(),
            },
            provider,
            conversation_history: vec![],
            reasoning_to_replay: None,
            item_sink: None,
        })
        .await
        .expect("turn should not error");

    // Mutating tool in Agent mode should trigger NeedsApproval.
    match result {
        TurnResult::NeedsApproval { tool_name, turn_id: _, .. } => {
            assert_eq!(tool_name, "write_file");
        }
        TurnResult::Completed { .. } => {
            // In trusted mode this would be ok, but we're in Agent mode.
            panic!("mutating tool in Agent mode should require approval");
        }
        other => panic!("expected NeedsApproval or Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn test_trusted_mode_auto_approves() {
    struct TestWriteTool;
    #[async_trait::async_trait]
    impl ToolHandler for TestWriteTool {
        fn name(&self) -> &str {
            "write_file"
        }
        fn is_mutating(&self) -> bool {
            true
        }
        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Function {
                body: Some(serde_json::json!({"written": 100})),
                success: true,
            })
        }
    }

    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TestWriteTool)).await;
        r
    });
    // Trusted mode.
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Trusted,
        PermissionProfile::new(),
    ));
    let events = Arc::new(EventBus::new());

    let runner = TurnRunner::new(registry, policy.clone(), events.clone());
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response(
            "write_file",
            serde_json::json!({"path": "out.txt", "content": "data"}),
        ),
        mock_text_response("File written successfully"),
    ]));

    let result = runner
        .run_turn(RunTurnRequest {
            thread_id: ThreadId::from_string("thread-trusted"),
            turn_id: TurnId::from_string("turn-trusted"),
            user_input: "Write data".into(),
            config: TurnConfig {
                model: "mock-model".into(),
                max_output_tokens: Some(100),
                token_budget: 10_000,
                turn_timeout: None,
                agent_mode: AgentMode::Trusted,
                system_prompt: None,
                workspace: "/tmp/test".into(),
            },
            provider,
            conversation_history: vec![],
            reasoning_to_replay: None,
            item_sink: None,
        })
        .await
        .expect("turn should complete");

    match result {
        TurnResult::Completed { answer, .. } => {
            assert!(answer.contains("File written"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn test_tool_execution_failure() {
    // A tool that always fails.
    struct FailingTool;
    #[async_trait::async_trait]
    impl ToolHandler for FailingTool {
        fn name(&self) -> &str {
            "shell_exec"
        }
        fn is_mutating(&self) -> bool {
            true
        }
        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed {
                message: "command not found".into(),
            })
        }
    }

    let registry = Arc::new({
        let mut r = ToolRegistry::new();
        r.register(Arc::new(FailingTool)).await;
        r
    });
    // Trusted mode to auto-approve.
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Trusted,
        PermissionProfile::new(),
    ));
    let events = Arc::new(EventBus::new());

    let runner = TurnRunner::new(registry, policy.clone(), events.clone());
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response("shell_exec", serde_json::json!({"command": "nonexistent"})),
        mock_text_response("The command failed"),
    ]));

    let result = runner
        .run_turn(RunTurnRequest {
            thread_id: ThreadId::from_string("thread-fail"),
            turn_id: TurnId::from_string("turn-fail"),
            user_input: "Run nonexistent command".into(),
            config: TurnConfig {
                model: "mock-model".into(),
                max_output_tokens: Some(100),
                token_budget: 10_000,
                turn_timeout: None,
                agent_mode: AgentMode::Trusted,
                system_prompt: None,
                workspace: "/tmp/test".into(),
            },
            provider,
            conversation_history: vec![],
            reasoning_to_replay: None,
            item_sink: None,
        })
        .await
        .expect("turn should complete even if tool fails");

    // The turn itself should complete; tool failures are reported to the model,
    // not propagated as turn errors.
    match result {
        TurnResult::Completed { .. } => {}
        other => panic!("turn with failed tool should still complete, got {other:?}"),
    }
}
