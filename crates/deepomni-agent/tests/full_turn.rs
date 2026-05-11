//! Integration tests for full agent turns with a mock provider.

use std::sync::Arc;

use deepomni_agent::{RunTurnRequest, TurnConfig, TurnResult, TurnRunner};
use deepomni_journal::InMemoryJournal;
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::tool::ToolOutput;
use deepomni_test_support::{mock_text_response, mock_tool_call_response, MockModelProvider};
use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};

struct TestReadTool;

#[async_trait::async_trait]
impl ToolHandler for TestReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Function {
            body: Some(serde_json::json!({"content": "hello world", "size": 11})),
            success: true,
        })
    }
}

async fn runner_with(handler: Arc<dyn ToolHandler>, mode: AgentMode) -> TurnRunner {
    let registry = Arc::new({
        let mut registry = ToolRegistry::new();
        registry.register(handler).await;
        registry
    });
    let policy = Arc::new(PolicyEngine::new(mode, PermissionProfile::new()));
    TurnRunner::new(registry, policy, Arc::new(InMemoryJournal::new()))
}

fn config(mode: AgentMode) -> TurnConfig {
    TurnConfig {
        model: "mock-model".into(),
        max_output_tokens: Some(100),
        token_budget: 10_000,
        turn_timeout: None,
        agent_mode: mode,
        system_prompt: None,
        workspace: "/tmp/test".into(),
    }
}

fn request(
    provider: Arc<MockModelProvider>,
    mode: AgentMode,
    user_input: &str,
) -> RunTurnRequest {
    RunTurnRequest {
        thread_id: ThreadId::new(),
        turn_id: TurnId::new(),
        user_input: user_input.into(),
        config: config(mode),
        provider,
        conversation_history: vec![],
        reasoning_to_replay: None,
        approval_store: None,
        item_sink: None,
    }
}

#[tokio::test]
async fn test_full_turn_text_only() {
    let runner = runner_with(Arc::new(TestReadTool), AgentMode::Agent).await;
    let provider = Arc::new(MockModelProvider::new(vec![mock_text_response(
        "The file contains hello world",
    )]));

    let result = runner
        .run_turn(request(provider, AgentMode::Agent, "What does the file contain?"))
        .await
        .expect("turn should complete");

    match result {
        TurnResult::Completed { answer, .. } => assert!(answer.contains("hello world")),
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn test_full_turn_with_tool_call() {
    let runner = runner_with(Arc::new(TestReadTool), AgentMode::Agent).await;
    let provider = Arc::new(MockModelProvider::new(vec![mock_tool_call_response(
        "read_file",
        serde_json::json!({"path": "src/main.rs"}),
    )]));

    let result = runner
        .run_turn(request(provider, AgentMode::Agent, "read src/main.rs"))
        .await
        .expect("turn should complete");

    assert!(matches!(result, TurnResult::Completed { .. }));
}

#[tokio::test]
async fn test_mutating_tool_requires_approval() {
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

    let runner = runner_with(Arc::new(TestWriteTool), AgentMode::Agent).await;
    let provider = Arc::new(MockModelProvider::new(vec![mock_tool_call_response(
        "write_file",
        serde_json::json!({"path": "out.txt", "content": "data"}),
    )]));

    let result = runner
        .run_turn(request(provider, AgentMode::Agent, "Write data to out.txt"))
        .await
        .expect("turn should not error");

    match result {
        TurnResult::NeedsApproval { tool_name, .. } => assert_eq!(tool_name, "write_file"),
        other => panic!("expected NeedsApproval, got {other:?}"),
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

    let runner = runner_with(Arc::new(TestWriteTool), AgentMode::Trusted).await;
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response(
            "write_file",
            serde_json::json!({"path": "out.txt", "content": "data"}),
        ),
        mock_text_response("File written successfully"),
    ]));

    let result = runner
        .run_turn(request(provider, AgentMode::Trusted, "Write data"))
        .await
        .expect("turn should complete");

    match result {
        TurnResult::Completed { answer, .. } => assert!(answer.contains("File written")),
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn test_tool_execution_failure_still_completes() {
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

    let runner = runner_with(Arc::new(FailingTool), AgentMode::Trusted).await;
    let provider = Arc::new(MockModelProvider::new(vec![
        mock_tool_call_response("shell_exec", serde_json::json!({"command": "missing"})),
        mock_text_response("The command failed"),
    ]));

    let result = runner
        .run_turn(request(provider, AgentMode::Trusted, "Run missing command"))
        .await
        .expect("turn should complete even if tool fails");

    assert!(matches!(result, TurnResult::Completed { .. }));
}
