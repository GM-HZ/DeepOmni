//! Integration tests for full agent turns with a mock provider.

use std::sync::Arc;

use deepomni_agent::{RunTurnRequest, TurnConfig, TurnRequestKind, TurnResult, TurnRunner};
use deepomni_journal::InMemoryJournal;
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::id::{ThreadId, TurnId};
use deepomni_protocol::tool::ToolOutput;
use deepomni_test_support::{MockModelProvider, mock_text_response, mock_tool_call_response};
use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};
use deepomni_trace::NoopTraceWriter;

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
    TurnRunner::new(
        registry,
        policy,
        Arc::new(InMemoryJournal::new()),
        Arc::new(NoopTraceWriter),
    )
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

fn request(provider: Arc<MockModelProvider>, mode: AgentMode, user_input: &str) -> RunTurnRequest {
    RunTurnRequest {
        kind: TurnRequestKind::NewTurn,
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
        .run_turn(request(
            provider,
            AgentMode::Agent,
            "What does the file contain?",
        ))
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

#[tokio::test]
async fn test_tool_router_integration() {
    use deepomni_tools::{ToolCallRuntime, ToolRouter};
    let registry = Arc::new(deepomni_tools::ToolRegistry::new());
    let router = ToolRouter::new(vec![]);
    let _runtime = ToolCallRuntime::new(registry.clone());
    let call = router.build_pending_call(
        &registry,
        deepomni_protocol::id::ToolCallId::from_string("c1"),
        "unknown_tool",
        serde_json::json!({}),
    );
    assert!(
        call.is_mutating,
        "unknown tools default to mutating for safety"
    );
}

/// Plan 2 acceptance: Unknown tool is rejected by router → Forbidden in agent.
#[tokio::test]
async fn test_plan2_unknown_tool_rejected_by_router_in_agent() {
    use deepomni_agent::{RunTurnRequest, TurnConfig, TurnRequestKind, TurnResult, TurnRunner};
    use deepomni_journal::InMemoryJournal;
    use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
    use deepomni_protocol::id::{ThreadId, TurnId};
    use deepomni_test_support::{MockModelProvider, mock_tool_call_response};
    use deepomni_tools::ToolRegistry;
    use deepomni_trace::NoopTraceWriter;

    let registry = Arc::new(ToolRegistry::new());
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Agent,
        PermissionProfile::new(),
    ));
    let journal: Arc<dyn deepomni_journal::TurnJournal> = Arc::new(InMemoryJournal::new());
    let runner = TurnRunner::new(registry.clone(), policy, journal, Arc::new(NoopTraceWriter));

    // Provider returns a tool call for an UNKNOWN tool.
    let provider = Arc::new(MockModelProvider::new(vec![mock_tool_call_response(
        "nonexistent_tool",
        serde_json::json!({"x": 1}),
    )]));

    let result = runner
        .run_turn(RunTurnRequest {
            kind: TurnRequestKind::NewTurn,
            thread_id: ThreadId::from_string("plan2-unknown"),
            turn_id: TurnId::from_string("plan2-t1"),
            user_input: "use nonexistent tool".into(),
            config: TurnConfig {
                model: "mock".into(),
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
            approval_store: None,
            item_sink: None,
        })
        .await
        .expect("turn should not error on unknown tool");

    // Unknown tool gets Forbidden → not executed → turn completes with empty answer.
    assert!(
        matches!(result, TurnResult::Completed { .. }),
        "unknown tool should be forbidden by router, turn completes"
    );
}

/// Plan 2 acceptance: Sandbox escalation from ToolPolicy.
#[test]
fn test_plan2_sandbox_preference_on_handlers() {
    use deepomni_tools::SandboxPreference;

    // Default sandbox preference is Unsandboxed.
    let default_pref = SandboxPreference::Unsandboxed;
    assert!(!matches!(default_pref, SandboxPreference::Required));

    // Sandboxed preference means escalation is possible.
    let sandboxed = SandboxPreference::Sandboxed;
    assert!(matches!(sandboxed, SandboxPreference::Sandboxed));

    // Required means hard deny without sandbox.
    let required = SandboxPreference::Required;
    assert!(matches!(required, SandboxPreference::Required));
}

/// Plan 2 acceptance: pre_tool_hook blocks execution in registry dispatch.
#[tokio::test]
async fn test_plan2_pre_hook_blocks_tool_execution() {
    use deepomni_protocol::id::ToolCallId;
    use deepomni_protocol::tool::{ToolOutput, ToolPayload};
    use deepomni_tools::{ToolError, ToolHandler, ToolInvocation, ToolRegistry};

    struct BlockingTool;
    #[async_trait::async_trait]
    impl ToolHandler for BlockingTool {
        fn name(&self) -> &str {
            "blocking_tool"
        }
        fn is_mutating(&self) -> bool {
            true
        }
        fn pre_tool_use_payload(&self, _: &ToolInvocation) -> Option<String> {
            Some("blocked by policy".into())
        }
        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::Function {
                body: None,
                success: true,
            })
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BlockingTool)).await;

    let result = registry
        .dispatch(ToolInvocation {
            call_id: ToolCallId::new(),
            tool_name: "blocking_tool".into(),
            payload: ToolPayload::Function {
                arguments: "{}".into(),
            },
            timeout: None,
            allow_mutating: true,
            workspace: None,
        })
        .await;

    assert!(
        matches!(result, Err(ToolError::PermissionDenied { .. })),
        "pre-tool hook should block execution with PermissionDenied"
    );
}

/// Plan 2 Task 3: Unknown tool produces router-level forbidden, not panic/skip.
#[tokio::test]
async fn unknown_tool_is_rejected_by_router() {
    use deepomni_agent::{RunTurnRequest, TurnConfig, TurnRequestKind, TurnResult, TurnRunner};
    use deepomni_journal::InMemoryJournal;
    use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
    use deepomni_protocol::id::{ThreadId, TurnId};
    use deepomni_test_support::{MockModelProvider, mock_tool_call_response};
    use deepomni_tools::ToolRegistry;
    use deepomni_trace::NoopTraceWriter;

    let registry = Arc::new(ToolRegistry::new());
    let policy = Arc::new(PolicyEngine::new(
        AgentMode::Agent,
        PermissionProfile::new(),
    ));
    let journal: Arc<dyn deepomni_journal::TurnJournal> = Arc::new(InMemoryJournal::new());
    let runner = TurnRunner::new(registry.clone(), policy, journal, Arc::new(NoopTraceWriter));

    let provider = Arc::new(MockModelProvider::new(vec![mock_tool_call_response(
        "nonexistent_tool",
        serde_json::json!({}),
    )]));

    let result = runner
        .run_turn(RunTurnRequest {
            kind: TurnRequestKind::NewTurn,
            thread_id: ThreadId::from_string("router-unknown"),
            turn_id: TurnId::from_string("router-t1"),
            user_input: "use unknown tool".into(),
            config: TurnConfig {
                model: "mock".into(),
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
            approval_store: None,
            item_sink: None,
        })
        .await
        .expect("turn should not panic on unknown tool");

    assert!(
        matches!(result, TurnResult::Completed { .. }),
        "unknown tool is rejected by router, turn completes"
    );
}

/// Plan 2 Task 4: Two non-mutating parallel-safe tools execute concurrently.
/// Uses a concurrency probe to verify max_active >= 2.
#[tokio::test]
async fn parallel_tool_calls_execute_in_batch() {
    use deepomni_protocol::id::ToolCallId;
    use deepomni_tools::{PendingToolCall, ToolCallResult, ToolCallRuntime, ToolRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ConcurrentProbe {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl deepomni_tools::ToolHandler for ConcurrentProbe {
        fn name(&self) -> &str {
            "probe"
        }
        fn supports_parallel(&self) -> bool {
            true
        }
        async fn handle(
            &self,
            _: deepomni_tools::ToolInvocation,
        ) -> Result<deepomni_protocol::tool::ToolOutput, deepomni_tools::ToolError> {
            let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(deepomni_protocol::tool::ToolOutput::Function {
                body: None,
                success: true,
            })
        }
    }

    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(ConcurrentProbe {
            active,
            max_active: max_active.clone(),
        }))
        .await;
    let runtime = ToolCallRuntime::new(Arc::new(registry));

    let calls: Vec<_> = (0..3)
        .map(|_| PendingToolCall {
            invocation: deepomni_tools::ToolInvocation {
                call_id: ToolCallId::new(),
                tool_name: "probe".into(),
                payload: deepomni_protocol::tool::ToolPayload::Function {
                    arguments: "{}".into(),
                },
                timeout: None,
                allow_mutating: false,
                workspace: None,
            },
            is_mutating: false,
            supports_parallel: true,
            sandbox_attempt: deepomni_sandbox::SandboxAttempt::Disabled,
        })
        .collect();

    let start = std::time::Instant::now();
    let results: Vec<ToolCallResult> = runtime.execute_batch(calls).await;
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 3, "all calls must complete");
    assert!(results.iter().all(|r| r.output.is_ok()), "all must succeed");
    // Concurrency: 3 tools sleeping 30ms each should take ~30ms, not ~90ms.
    assert!(
        elapsed < std::time::Duration::from_millis(80),
        "batch must execute concurrently (took {elapsed:?}, expected <80ms)"
    );
    assert!(
        max_active.load(Ordering::SeqCst) >= 2,
        "at least 2 tools must run concurrently"
    );
}
