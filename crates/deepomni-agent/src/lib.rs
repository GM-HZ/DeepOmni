//! # DeepOmni Agent
//!
//! Agent loop and turn runner. Executes the core turn sequence:
//! input → context → provider → streaming → tool detection → orchestration
//! → approval → execution → continuation → completion.
//!
//! Implements DeepSeek reasoning handling and sub-agent control boundary.
//! PRD §7.4, §8.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::info;

use deepomni_context::{ContextFragment, ContextManager, TokenBudget, assemble_context};
use deepomni_engine::{AgentControl, Mailbox};
use deepomni_journal::{JournalEntry, TurnJournal};
use deepomni_model_provider::{
    MessageRole, ModelDelta, ModelMessage, ModelProvider, ModelRequest, ModelToolCall,
    ReasoningReplay,
};
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::EventFrame;
use deepomni_protocol::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};
use deepomni_tools::{
    ApprovalStore, PendingToolCall, ToolCallResult, ToolCallRuntime, ToolRegistry, ToolRouter,
};
use deepomni_trace::TraceWriter;

/// Per-turn configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TurnConfig {
    pub model: String,
    pub max_output_tokens: Option<u64>,
    pub token_budget: u64,
    pub turn_timeout: Option<Duration>,
    pub agent_mode: AgentMode,
    pub system_prompt: Option<String>,
    pub workspace: String,
}

/// Frozen snapshot of all context a turn needs. Created once at turn start
/// and shared immutably throughout the entire turn lifecycle. Pattern from
/// Codex `TurnContext` / Claude Code `ToolUseContext`.
#[derive(Clone)]
pub struct TurnExecutionContext {
    pub config: TurnConfig,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub user_input: String,
    pub tool_specs: Vec<deepomni_protocol::tool::ToolSpec>,
    /// Callback for persisting structured turn items to durable state.
    pub item_sink: Option<Arc<dyn Fn(deepomni_protocol::TurnItem) + Send + Sync>>,
}

impl std::fmt::Debug for TurnExecutionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnExecutionContext")
            .field("config", &self.config)
            .field("thread_id", &self.thread_id)
            .field("turn_id", &self.turn_id)
            .field("has_item_sink", &self.item_sink.is_some())
            .finish()
    }
}

impl TurnExecutionContext {
    pub fn workspace(&self) -> &str {
        &self.config.workspace
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    fn record(&self, item: deepomni_protocol::TurnItem) {
        if let Some(ref sink) = self.item_sink {
            sink(item);
        }
    }
}

/// Expanded frozen per-turn snapshot used by the new engine boundary. The
/// current runner still accepts `RunTurnRequest`/`ResumeTurnRequest`; this is
/// the migration target for collapsing those inputs into one immutable object.
#[derive(Clone)]
pub struct TurnContext {
    pub config: TurnConfig,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub user_input: String,
    pub tool_specs: Vec<deepomni_protocol::tool::ToolSpec>,
    pub provider: Arc<dyn ModelProvider>,
    pub approval_policy: AgentMode,
    pub permission_profile: PermissionProfile,
    pub cwd: PathBuf,
    pub environment_vars: HashMap<String, String>,
    pub turn_start_time: Instant,
    pub token_budget: TokenBudget,
    pub context_manager: ContextManager,
    pub journal: Arc<dyn TurnJournal>,
    pub reasoning_to_replay: Option<Vec<ReasoningReplay>>,
}

impl std::fmt::Debug for TurnContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnContext")
            .field("config", &self.config)
            .field("thread_id", &self.thread_id)
            .field("turn_id", &self.turn_id)
            .field("tool_specs_len", &self.tool_specs.len())
            .field("provider", &self.provider.provider_name())
            .field("approval_policy", &self.approval_policy)
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl TurnContext {
    pub fn from_run_request(
        request: RunTurnRequest,
        tool_specs: Vec<deepomni_protocol::tool::ToolSpec>,
        permission_profile: PermissionProfile,
        journal: Arc<dyn TurnJournal>,
    ) -> Self {
        let cwd = PathBuf::from(&request.config.workspace);
        let token_budget = TokenBudget::new(request.config.token_budget);
        let context_manager = ContextManager::from_items(request.conversation_history);
        Self {
            approval_policy: request.config.agent_mode,
            config: request.config,
            thread_id: request.thread_id,
            turn_id: request.turn_id,
            user_input: request.user_input,
            tool_specs,
            provider: request.provider,
            permission_profile,
            cwd,
            environment_vars: std::env::vars().collect(),
            turn_start_time: Instant::now(),
            token_budget,
            context_manager,
            journal,
            reasoning_to_replay: request.reasoning_to_replay,
        }
    }

    pub fn prompt_history(&self) -> Vec<ModelMessage> {
        self.context_manager.for_prompt()
    }
}

/// Request to run a new turn. Bundles all parameters that were previously
/// spread across `run_turn`'s long argument list.
pub struct RunTurnRequest {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub user_input: String,
    pub config: TurnConfig,
    pub provider: Arc<dyn ModelProvider>,
    pub conversation_history: Vec<ModelMessage>,
    pub reasoning_to_replay: Option<Vec<ReasoningReplay>>,
    pub approval_store: Option<Arc<ApprovalStore>>,
    pub item_sink: Option<Arc<dyn Fn(deepomni_protocol::TurnItem) + Send + Sync>>,
}

/// Request to resume a turn after tool approval. Carries the tool result
/// and the turn transcript so the continuation can reconstruct the full
/// conversation without treating the tool result as a new user message.
pub struct ResumeTurnRequest {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub config: TurnConfig,
    pub provider: Arc<dyn ModelProvider>,
    pub tool_result_content: String,
    pub transcript: Vec<ModelMessage>,
    pub reasoning_to_replay: Option<Vec<ReasoningReplay>>,
    pub item_sink: Option<Arc<dyn Fn(deepomni_protocol::TurnItem) + Send + Sync>>,
}

// Phase E: RequestCompiler unified into deepomni-engine crate.
// Agent now uses deepomni_engine::RequestCompiler directly.

/// The core turn runner.
///
/// Each turn follows PRD §8 sequence: receive input → persist →
/// build capability snapshot → assemble context → stream →
/// detect tools → orchestrate → approve → execute → continue → complete.
pub struct TurnRunner {
    tool_registry: Arc<ToolRegistry>,
    #[allow(dead_code)]
    policy_engine: Arc<PolicyEngine>,
    journal: Arc<dyn TurnJournal>,
    tool_router: ToolRouter,
    tool_call_runtime: ToolCallRuntime,
    trace_writer: Arc<dyn TraceWriter>,
    agent_control: Option<Arc<AgentControl>>,
}

impl TurnRunner {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        policy_engine: Arc<PolicyEngine>,
        journal: Arc<dyn TurnJournal>,
        trace_writer: Arc<dyn TraceWriter>,
    ) -> Self {
        let tool_router = ToolRouter::new(Vec::new());
        let tool_call_runtime = ToolCallRuntime::new(tool_registry.clone());
        Self {
            tool_registry,
            policy_engine,
            journal,
            tool_router,
            tool_call_runtime,
            trace_writer,
            agent_control: None,
        }
    }

    /// Set the agent control for multi-agent spawn support.
    pub fn with_agent_control(mut self, agent_control: Arc<AgentControl>) -> Self {
        self.agent_control = Some(agent_control);
        self
    }

    /// Run a single turn: send user input to the model, stream output,
    /// handle tool calls, and return the final result.
    pub async fn run_turn(&self, request: RunTurnRequest) -> Result<TurnResult, TurnError> {
        let RunTurnRequest {
            thread_id,
            turn_id,
            user_input,
            config,
            provider,
            conversation_history,
            reasoning_to_replay,
            approval_store,
            item_sink,
        } = request;

        // 1. Build tool specs and freeze the turn execution context.
        let tool_specs = self.tool_registry.list_specs().await;
        let ctx = TurnExecutionContext {
            config: config.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            user_input: user_input.clone(),
            tool_specs: tool_specs.clone(),
            item_sink,
        };
        ctx.record(deepomni_protocol::TurnItem::UserMessage {
            content: user_input.clone(),
        });

        // 3. Build context fragments and assemble under token budget.
        let fragments =
            Self::build_context_fragments(&config, &user_input, &conversation_history, &tool_specs);
        let assembled = assemble_context(turn_id.clone(), fragments, ctx.config.token_budget);

        // Emit budget debug metadata.
        let budget_debug = assembled.budget.debug_metadata();
        tracing::debug!(
            token_budget_total = budget_debug.total,
            token_budget_consumed = budget_debug.consumed,
            token_budget_remaining = budget_debug.remaining,
            overflow_actions = assembled.overflow_actions.len(),
            "context assembled"
        );

        // Emit compaction events if overflow occurred.
        if !assembled.overflow_actions.is_empty() {
            self.emit(
                thread_id.clone(),
                EventFrame::ContextCompactionStarted {
                    turn_id: turn_id.clone(),
                },
            )
            .await;
            self.emit(
                thread_id.clone(),
                EventFrame::ContextCompactionCompleted {
                    turn_id: turn_id.clone(),
                },
            )
            .await;
        }

        // 4. Build initial model request using engine's unified RequestCompiler (Phase E).
        let compile_config = deepomni_engine::CompileConfig {
            system_prompt: config.system_prompt.as_deref(),
            fragments: &assembled.messages,
            tools: tool_specs.clone(),
            max_output_tokens: ctx.config.max_output_tokens,
            model: ctx.model(),
            reasoning_to_replay: reasoning_to_replay.clone(),
        };
        let compiled = if conversation_history.is_empty() {
            deepomni_engine::RequestCompiler::compile_new_turn(
                &compile_config,
                &conversation_history,
                &user_input,
            )
        } else {
            deepomni_engine::RequestCompiler::compile_tool_continuation(
                &compile_config,
                &conversation_history,
            )
        };
        let request = compiled.request;

        // 5. Stream from provider in a continuation loop.
        // Maintain an append-only transcript: every iteration appends
        // assistant(tool_calls) + tool results, preserving full history.
        let mut transcript = request.messages.clone();
        let message_id = MessageId::new();
        let mut final_answer = String::new();
        let mut iteration = 0u32;
        const MAX_ITERATIONS: u32 = 10;
        let mut pending_tool_calls: Vec<ToolCallState> = Vec::new();
        let mut current_request = request;
        let mut iteration_tool_calls: Vec<ModelToolCall> = Vec::new();
        // Phase 3: Per-iteration pending call batch for ToolCallRuntime.
        let mut iteration_pending_calls: Vec<PendingToolCall> = Vec::new();

        loop {
            iteration += 1;
            if iteration > MAX_ITERATIONS {
                break;
            }

            // Phase 4: Trace inference attempt.
            self.trace_writer
                .record_inference_attempt(&thread_id, &turn_id, ctx.model());

            let mut stream = provider
                .stream(current_request)
                .await
                .map_err(|e| TurnError::ProviderError(format!("{e}")))?;

            let mut active_tool_call: Option<ToolCallState> = None;
            let mut current_reasoning: Option<ReasoningState> = None;
            let mut iteration_tool_results: Vec<ModelMessage> = Vec::new();
            let mut had_tool_call_this_iteration = false;

            loop {
                let delta = tokio::time::timeout(
                    Duration::from_secs(120),
                    futures::StreamExt::next(&mut stream),
                )
                .await
                .map_err(|_| TurnError::Timeout)?
                .transpose()
                .map_err(|e| TurnError::ProviderError(format!("{e}")))?;

                match delta {
                    None => break,
                    Some(ModelDelta::End) => break,
                    Some(ModelDelta::Text(text)) => {
                        final_answer.push_str(&text);
                        ctx.record(deepomni_protocol::TurnItem::AssistantDelta {
                            delta: text.clone(),
                        });
                        self.emit(
                            thread_id.clone(),
                            EventFrame::AssistantMessageDelta {
                                turn_id: turn_id.clone(),
                                message_id: message_id.clone(),
                                delta: text,
                            },
                        )
                        .await;
                    }
                    Some(ModelDelta::Reasoning {
                        delta,
                        replay_required,
                    }) => {
                        if current_reasoning.is_none() {
                            current_reasoning = Some(ReasoningState {
                                content: String::new(),
                                replay_required,
                            });
                        }
                        if let Some(ref mut rs) = current_reasoning {
                            rs.content.push_str(&delta);
                        }
                        self.emit(
                            thread_id.clone(),
                            EventFrame::AssistantReasoningDelta {
                                turn_id: turn_id.clone(),
                                message_id: message_id.clone(),
                                delta,
                                replay_required,
                                visibility:
                                    deepomni_protocol::event::ReasoningVisibility::HostRenderable,
                            },
                        )
                        .await;
                    }
                    Some(ModelDelta::ToolCallStart { call_id, tool_name }) => {
                        active_tool_call = Some(ToolCallState {
                            call_id: ToolCallId::from_string(&call_id),
                            tool_name,
                            arguments: String::new(),
                        });
                        let tc_id = ToolCallId::from_string(&call_id);
                        let tc_name = active_tool_call.as_ref().unwrap().tool_name.clone();
                        self.emit(
                            thread_id.clone(),
                            EventFrame::ToolCallStarted {
                                turn_id: turn_id.clone(),
                                call_id: tc_id.clone(),
                                tool_name: tc_name.clone(),
                            },
                        )
                        .await;
                        // Write to journal for SSE replay (Plan 2).
                        let _ = self.journal.append(
                            &thread_id,
                            &turn_id,
                            JournalEntry::ToolCallRequested {
                                call_id: tc_id,
                                tool_name: tc_name,
                                arguments: serde_json::Value::Null,
                            },
                        );
                    }
                    Some(ModelDelta::ToolCallArgumentsDelta { call_id, delta }) => {
                        if let Some(ref mut tc) = active_tool_call
                            && tc.call_id.as_str() == call_id
                        {
                            tc.arguments.push_str(&delta);
                        }
                        self.emit(
                            thread_id.clone(),
                            EventFrame::ToolCallArgumentsDelta {
                                turn_id: turn_id.clone(),
                                call_id: ToolCallId::from_string(&call_id),
                                delta,
                            },
                        )
                        .await;
                    }
                    Some(ModelDelta::ToolCallComplete {
                        call_id: _,
                        tool_name: _,
                        arguments,
                    }) => {
                        if let Some(tc) = active_tool_call.take() {
                            had_tool_call_this_iteration = true;

                            if let Some(ref rs) = current_reasoning {
                                ctx.record(deepomni_protocol::TurnItem::ReasoningBlock {
                                    content: rs.content.clone(),
                                    replay_required: rs.replay_required,
                                });
                                self.emit(
                                    thread_id.clone(),
                                    EventFrame::AssistantReasoningCompleted {
                                        turn_id: turn_id.clone(),
                                        message_id: message_id.clone(),
                                        replay_required: rs.replay_required,
                                    },
                                )
                                .await;
                            }

                            // Phase 3: Build PendingToolCall via ToolRouter and collect for batch execution.
                            let pending = self.tool_router.build_pending_call(
                                &self.tool_registry,
                                tc.call_id.clone(),
                                &tc.tool_name,
                                arguments.clone(),
                            );
                            pending_tool_calls.push(tc.clone());
                            iteration_tool_calls.push(ModelToolCall {
                                call_id: tc.call_id.to_string(),
                                tool_name: tc.tool_name.clone(),
                                arguments: serde_json::from_str(&tc.arguments).unwrap_or_default(),
                            });
                            ctx.record(deepomni_protocol::TurnItem::ToolCall {
                                call_id: tc.call_id.clone(),
                                tool_name: tc.tool_name.clone(),
                                arguments: arguments.clone(),
                            });

                            // Pre-flight approval check for this call.
                            let approval_key = serde_json::json!({
                                "tool_name": tc.tool_name.clone(),
                                "arguments": arguments.clone(),
                            });
                            let handler = self.tool_registry.get(&tc.tool_name);
                            let decision = if let Some(h) = handler {
                                if let Some(store) = approval_store.as_ref() {
                                    deepomni_tools::ToolOrchestrator::evaluate_with_cache(
                                        h.as_ref(),
                                        &ctx.config.agent_mode,
                                        store,
                                        &approval_key,
                                    )
                                } else {
                                    deepomni_tools::ToolOrchestrator::evaluate(
                                        h.as_ref(),
                                        &ctx.config.agent_mode,
                                    )
                                }
                            } else {
                                deepomni_tools::OrchestratorDecision::Forbidden {
                                    reason: format!("unknown tool: {}", tc.tool_name),
                                }
                            };

                            match decision {
                                deepomni_tools::OrchestratorDecision::Proceed { .. } => {
                                    // Phase 3: Collect for batch execution via ToolCallRuntime.
                                    iteration_pending_calls.push(pending);
                                }
                                deepomni_tools::OrchestratorDecision::NeedsApproval {
                                    reason,
                                    approval_id,
                                } => {
                                    let _ = self.journal.append(
                                        &thread_id,
                                        &turn_id,
                                        JournalEntry::ApprovalPending {
                                            call_id: tc.call_id.clone(),
                                            approval_id: approval_id.clone(),
                                            tool_name: tc.tool_name.clone(),
                                            arguments: arguments.clone(),
                                            reason: reason.clone(),
                                            model: ctx.config.model.clone(),
                                            workspace: ctx.config.workspace.clone(),
                                            config_json: serde_json::to_string(&ctx.config)
                                                .unwrap_or_default(),
                                        },
                                    );
                                    return Ok(TurnResult::NeedsApproval {
                                        turn_id,
                                        call_id: tc.call_id,
                                        tool_name: tc.tool_name,
                                        arguments,
                                        approval_id,
                                    });
                                }
                                deepomni_tools::OrchestratorDecision::Forbidden { reason } => {
                                    self.emit(
                                        thread_id.clone(),
                                        EventFrame::ToolCallFailed {
                                            turn_id: turn_id.clone(),
                                            call_id: tc.call_id.clone(),
                                            error: reason,
                                        },
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }

            // Record provider usage after stream ends (Plan 3 Task 3).
            // Note: current_request was already consumed by stream(), so we estimate from transcript.
            let prompt_estimate: u64 = transcript
                .iter()
                .map(|m| m.content.as_deref().unwrap_or("").len() as u64 / 4)
                .sum();
            let _ = self.journal.append(
                &thread_id,
                &turn_id,
                JournalEntry::ProviderUsage {
                    prompt_tokens: prompt_estimate,
                    completion_tokens: final_answer.len() as u64 / 4,
                },
            );

            // After stream ends, execute collected tool calls in batch via ToolCallRuntime (Phase 3).
            if had_tool_call_this_iteration {
                let batch = std::mem::take(&mut iteration_pending_calls);
                if !batch.is_empty() {
                    // Phase 4: Trace tool dispatch for each tool in the batch.
                    for call in &batch {
                        self.trace_writer.record_tool_dispatch(
                            &thread_id,
                            &turn_id,
                            &call.invocation.tool_name,
                        );
                    }

                    let results: Vec<ToolCallResult> =
                        self.tool_call_runtime.execute_batch(batch).await;

                    for result in &results {
                        let (success, preview) = match &result.output {
                            Ok(o) => match o {
                                deepomni_protocol::tool::ToolOutput::Function { body, success } => {
                                    (
                                        *success,
                                        body.as_ref().map(|b| b.to_string()).unwrap_or_default(),
                                    )
                                }
                                deepomni_protocol::tool::ToolOutput::Mcp { result: v } => {
                                    (true, v.to_string())
                                }
                            },
                            Err(_) => (false, String::new()),
                        };
                        ctx.record(deepomni_protocol::TurnItem::ToolResult {
                            call_id: result.call_id.clone(),
                            tool_name: result.tool_name.clone(),
                            output_preview: Some(preview.clone()),
                            success,
                        });
                        self.emit(
                            thread_id.clone(),
                            EventFrame::ToolCallCompleted {
                                turn_id: turn_id.clone(),
                                call_id: result.call_id.clone(),
                                tool_name: result.tool_name.clone(),
                                success,
                                output_preview: Some(preview.clone()),
                            },
                        )
                        .await;
                        // Write to journal for SSE replay (Plan 2).
                        if success {
                            let _ = self.journal.append(
                                &thread_id,
                                &turn_id,
                                JournalEntry::ToolCallCompleted {
                                    call_id: result.call_id.clone(),
                                    tool_name: result.tool_name.clone(),
                                    success: true,
                                    output: Some(preview.clone()),
                                    output_preview: Some(preview.clone()),
                                },
                            );
                        } else {
                            let _ = self.journal.append(
                                &thread_id,
                                &turn_id,
                                JournalEntry::ToolCallFailed {
                                    call_id: result.call_id.clone(),
                                    error: preview.clone(),
                                },
                            );
                        }

                        iteration_tool_results.push(ModelMessage {
                            role: MessageRole::Tool,
                            content: Some(if preview.is_empty() {
                                "tool executed".into()
                            } else {
                                preview
                            }),
                            tool_calls: vec![],
                            tool_call_id: Some(result.call_id.to_string()),
                            reasoning_content: None,
                        });
                    }

                    // Only continue if we have tool results to feed back.
                    if !iteration_tool_results.is_empty() {
                        let assistant_tool_calls = std::mem::take(&mut iteration_tool_calls);
                        transcript.push(ModelMessage {
                            role: MessageRole::Assistant,
                            content: if final_answer.is_empty() {
                                None
                            } else {
                                Some(final_answer.clone())
                            },
                            tool_calls: assistant_tool_calls,
                            tool_call_id: None,
                            reasoning_content: current_reasoning
                                .as_ref()
                                .map(|r| r.content.clone()),
                        });
                        transcript.extend(std::mem::take(&mut iteration_tool_results));

                        let reasoning_replay = current_reasoning.take().map(|r| {
                            vec![ReasoningReplay {
                                message_id: message_id.clone(),
                                reasoning_content: r.content,
                            }]
                        });

                        current_request = ModelRequest {
                            model: ctx.model().to_string(),
                            messages: transcript.clone(),
                            tools: tool_specs.clone(),
                            max_output_tokens: ctx.config.max_output_tokens,
                            temperature: None,
                            system_prompt: None,
                            replay_reasoning: reasoning_replay,
                        };
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // 6. Complete the turn.
        self.trace_writer
            .record_turn_completed(&thread_id, &turn_id);

        if !final_answer.is_empty() {
            self.emit(
                thread_id.clone(),
                EventFrame::AssistantMessageCompleted {
                    turn_id: turn_id.clone(),
                    message_id,
                },
            )
            .await;
        }

        Ok(TurnResult::Completed {
            turn_id,
            answer: final_answer,
            tool_calls: pending_tool_calls
                .into_iter()
                .map(|tc| CompletedToolCall {
                    call_id: tc.call_id,
                    tool_name: tc.tool_name,
                })
                .collect(),
        })
    }

    /// Continue a turn after the host approves a tool call.
    pub async fn continue_after_approval(
        &self,
        request: ResumeTurnRequest,
    ) -> Result<TurnResult, TurnError> {
        let ResumeTurnRequest {
            thread_id,
            turn_id,
            call_id,
            tool_name,
            arguments,
            config,
            provider,
            tool_result_content: _,
            transcript,
            reasoning_to_replay,
            item_sink,
        } = request;

        // Clone item_sink before moving into ctx; we need it for the run_turn call later.
        let item_sink_for_continuation = item_sink.clone();

        // Build frozen context for this continuation.
        let tool_specs = self.tool_registry.list_specs().await;
        let ctx = TurnExecutionContext {
            config: config.clone(),
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            user_input: String::new(),
            tool_specs,
            item_sink,
        };

        // Emit approval.
        self.emit(
            thread_id.clone(),
            EventFrame::ToolCallApproved {
                turn_id: turn_id.clone(),
                call_id: call_id.clone(),
            },
        )
        .await;

        // Phase 3: Execute the approved tool through ToolRouter + ToolCallRuntime.
        let pending = self.tool_router.build_pending_call(
            &self.tool_registry,
            call_id.clone(),
            &tool_name,
            arguments.clone(),
        );
        let batch_results = self.tool_call_runtime.execute_batch(vec![pending]).await;
        let result = batch_results
            .into_iter()
            .next()
            .ok_or_else(|| TurnError::ProviderError("tool execution produced no result".into()))?;

        let (success, preview, content) = match &result.output {
            Ok(o) => {
                let p = tool_output_preview(o);
                let c = p.clone();
                (true, Some(p), c)
            }
            Err(e) => (false, Some(format!("error: {e}")), format!("{e}")),
        };

        ctx.record(deepomni_protocol::TurnItem::ToolResult {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            output_preview: preview.clone(),
            success,
        });
        ctx.record(deepomni_protocol::TurnItem::ApprovalDecision {
            call_id: call_id.clone(),
            approved: true,
        });

        self.emit(
            thread_id.clone(),
            EventFrame::ToolCallCompleted {
                turn_id: turn_id.clone(),
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                success,
                output_preview: preview.clone(),
            },
        )
        .await;

        // Write to journal for SSE replay.
        if success {
            let _ = self.journal.append(
                &thread_id,
                &turn_id,
                JournalEntry::ToolCallCompleted {
                    call_id: call_id.clone(),
                    tool_name: tool_name.clone(),
                    success: true,
                    output: preview.clone(),
                    output_preview: preview.clone(),
                },
            );
        }

        // Continue with the full transcript + tool result, not a fake user input.
        let mut continuation_transcript = transcript;
        continuation_transcript.push(ModelMessage {
            role: MessageRole::Tool,
            content: Some(content),
            tool_calls: vec![],
            tool_call_id: Some(call_id.to_string()),
            reasoning_content: None,
        });

        self.run_turn(RunTurnRequest {
            thread_id,
            turn_id,
            user_input: String::new(),
            config,
            provider,
            conversation_history: continuation_transcript,
            reasoning_to_replay,
            approval_store: None,
            item_sink: item_sink_for_continuation,
        })
        .await
    }

    /// Emit an event through the event bus. Seq is assigned by the
    /// persistence layer; the agent passes a placeholder seq that is
    /// replaced when the runtime's state-backed emit path takes over.
    async fn emit(&self, thread_id: ThreadId, event: EventFrame) {
        if let Some((turn_id, entry)) = event_frame_to_journal_entry(&event) {
            let _ = self.journal.append(&thread_id, &turn_id, entry);
        }
    }

    /// Build context fragments for a turn.
    fn build_context_fragments(
        config: &TurnConfig,
        _user_input: &str,
        _history: &[ModelMessage],
        _tool_specs: &[deepomni_protocol::tool::ToolSpec],
    ) -> Vec<ContextFragment> {
        let mut fragments = Vec::new();

        // System prompt (protected).
        if let Some(ref sys) = config.system_prompt {
            fragments.push(ContextFragment::System {
                content: sys.clone(),
            });
        }

        // Environment info.
        fragments.push(ContextFragment::Environment {
            content: format!("workspace: {}", config.workspace),
        });

        // Conversation history will be added by the runtime layer.
        // Tool schemas will be rendered by the runtime layer.

        fragments
    }

    // ── Sub-agent spawning ──

    /// Spawn a child sub-agent turn.
    pub async fn spawn_subagent(
        &self,
        parent_thread_id: ThreadId,
        parent_turn_id: TurnId,
        task: String,
        provider: Arc<dyn ModelProvider>,
        config: TurnConfig,
    ) -> Result<SubagentResult, TurnError> {
        let subagent_id = SubagentId::new();
        let child_thread_id = ThreadId::new();
        let child_turn_id = TurnId::new();

        // Phase 5: Register child with AgentControl if available.
        let (mailbox, _receiver) = Mailbox::new();
        if let Some(ref ctrl) = self.agent_control
            && !ctrl.can_spawn(0)
        {
            return Err(TurnError::ProviderError("agent spawn limit reached".into()));
        }

        self.emit(
            parent_thread_id.clone(),
            EventFrame::SubagentSpawned {
                parent_turn_id: parent_turn_id.clone(),
                subagent_id: subagent_id.clone(),
                task: task.clone(),
            },
        )
        .await;

        // Write journal entry for SSE replay (Plan 4).
        let _ = self.journal.append(
            &parent_thread_id,
            &parent_turn_id,
            JournalEntry::SubagentSpawned {
                parent_turn_id: parent_turn_id.clone(),
                subagent_id: subagent_id.clone(),
                task: task.clone(),
            },
        );

        // Trace agent spawn.
        self.trace_writer
            .record_agent_spawn(&parent_thread_id, &child_thread_id);

        info!(
            subagent_id = %subagent_id,
            parent_turn = %parent_turn_id,
            "spawning sub-agent"
        );

        // Sub-agent runs with same tool set but stricter permissions.
        let result = self
            .run_turn(RunTurnRequest {
                thread_id: child_thread_id,
                turn_id: child_turn_id.clone(),
                user_input: task,
                config,
                provider,
                conversation_history: vec![],
                reasoning_to_replay: None,
                approval_store: None,
                item_sink: None,
            })
            .await;

        // Phase 5: Send result to parent via mailbox.
        match result {
            Ok(TurnResult::Completed { answer, .. }) => {
                let _ = mailbox.send(deepomni_protocol::agent::InterAgentMessage {
                    author: deepomni_protocol::agent::AgentPath::from_string(format!(
                        "/root/{}",
                        subagent_id
                    )),
                    recipient: deepomni_protocol::agent::AgentPath::root(),
                    other_recipients: vec![],
                    content: answer.clone(),
                    trigger_turn: true,
                });
                self.emit(
                    parent_thread_id.clone(),
                    EventFrame::SubagentCompleted {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        result_summary: answer.clone(),
                    },
                )
                .await;
                let _ = self.journal.append(
                    &parent_thread_id,
                    &parent_turn_id,
                    JournalEntry::SubagentCompleted {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        result_summary: answer.clone(),
                    },
                );
                Ok(SubagentResult {
                    subagent_id,
                    answer,
                })
            }
            Ok(other) => {
                let err_msg = format!("unexpected turn result: {other:?}");
                self.emit(
                    parent_thread_id.clone(),
                    EventFrame::SubagentFailed {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        error: err_msg.clone(),
                    },
                )
                .await;
                let _ = self.journal.append(
                    &parent_thread_id,
                    &parent_turn_id,
                    JournalEntry::SubagentFailed {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        error: err_msg,
                    },
                );
                Ok(SubagentResult {
                    subagent_id,
                    answer: "sub-agent did not complete normally".into(),
                })
            }
            Err(e) => {
                let err_msg = format!("{e}");
                self.emit(
                    parent_thread_id.clone(),
                    EventFrame::SubagentFailed {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        error: err_msg.clone(),
                    },
                )
                .await;
                let _ = self.journal.append(
                    &parent_thread_id,
                    &parent_turn_id,
                    JournalEntry::SubagentFailed {
                        parent_turn_id: parent_turn_id.clone(),
                        subagent_id: subagent_id.clone(),
                        error: err_msg,
                    },
                );
                Err(e)
            }
        }
    }
}

// ── Supporting types ──

#[derive(Debug, Clone)]
struct ToolCallState {
    call_id: ToolCallId,
    tool_name: String,
    arguments: String,
}

#[derive(Debug, Clone)]
struct ReasoningState {
    content: String,
    replay_required: bool,
}

#[derive(Debug, Clone)]
pub struct CompletedToolCall {
    pub call_id: ToolCallId,
    pub tool_name: String,
}

#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub subagent_id: SubagentId,
    pub answer: String,
}

/// Result of running a turn.
#[derive(Debug)]
pub enum TurnResult {
    Completed {
        turn_id: TurnId,
        answer: String,
        tool_calls: Vec<CompletedToolCall>,
    },
    NeedsApproval {
        turn_id: TurnId,
        call_id: ToolCallId,
        tool_name: String,
        arguments: serde_json::Value,
        approval_id: String,
    },
}

/// Errors during turn execution.
#[derive(Debug)]
pub enum TurnError {
    ProviderError(String),
    Timeout,
    UnknownTool(String),
    PolicyDenied(String),
    Cancelled,
    SubagentError(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TurnError::ProviderError(msg) => write!(f, "provider error: {msg}"),
            TurnError::Timeout => write!(f, "turn timed out"),
            TurnError::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            TurnError::PolicyDenied(msg) => write!(f, "policy denied: {msg}"),
            TurnError::Cancelled => write!(f, "turn cancelled"),
            TurnError::SubagentError(msg) => write!(f, "sub-agent error: {msg}"),
        }
    }
}

impl std::error::Error for TurnError {}

/// Extract a preview string from a tool output.
/// Normalize a conversation transcript: ensure every tool call has its result,
/// and remove orphan tool results (results without a matching call).
/// This prevents API errors when compaction drops items asymmetrically.
pub fn normalize_transcript(messages: &mut Vec<ModelMessage>) {
    // Collect all known call_ids from assistant tool_calls.
    let known_calls: std::collections::HashSet<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::Assistant)
        .flat_map(|m| m.tool_calls.iter().map(|tc| tc.call_id.clone()))
        .collect();

    // Remove orphan tool results (no matching call).
    messages.retain(|m| {
        if m.role != MessageRole::Tool {
            return true;
        }
        if let Some(ref cid) = m.tool_call_id {
            known_calls.contains(cid)
        } else {
            false
        }
    });

    // For assistant messages with tool_calls but no matching results,
    // this is acceptable — the model may not have executed those tools yet.
}

fn tool_output_preview(output: &deepomni_protocol::tool::ToolOutput) -> String {
    match output {
        deepomni_protocol::tool::ToolOutput::Function { body, .. } => body
            .as_ref()
            .map(|v| {
                let s = serde_json::to_string(v).unwrap_or_default();
                if s.len() > 200 {
                    format!("{}...", &s[..200])
                } else {
                    s
                }
            })
            .unwrap_or_else(|| "ok".into()),
        deepomni_protocol::tool::ToolOutput::Mcp { result } => {
            let s = serde_json::to_string(result).unwrap_or_default();
            if s.len() > 200 {
                format!("{}...", &s[..200])
            } else {
                s
            }
        }
    }
}

fn event_frame_to_journal_entry(event: &EventFrame) -> Option<(TurnId, JournalEntry)> {
    match event {
        EventFrame::TurnStarted {
            turn_id,
            user_input,
            ..
        } => Some((
            turn_id.clone(),
            JournalEntry::TurnStarted {
                user_input: user_input.clone(),
            },
        )),
        EventFrame::TurnCompleted { turn_id } => {
            Some((turn_id.clone(), JournalEntry::TurnCompleted))
        }
        EventFrame::TurnFailed { turn_id, error } => Some((
            turn_id.clone(),
            JournalEntry::TurnFailed {
                error: error.clone(),
            },
        )),
        EventFrame::AssistantMessageDelta {
            turn_id,
            message_id,
            delta,
        } => Some((
            turn_id.clone(),
            JournalEntry::AssistantDelta {
                message_id: message_id.clone(),
                delta: delta.clone(),
            },
        )),
        EventFrame::AssistantMessageCompleted {
            turn_id,
            message_id,
        } => Some((
            turn_id.clone(),
            JournalEntry::AssistantCompleted {
                message_id: message_id.clone(),
            },
        )),
        EventFrame::AssistantReasoningDelta {
            turn_id,
            message_id,
            delta,
            replay_required,
            ..
        } => Some((
            turn_id.clone(),
            JournalEntry::ReasoningDelta {
                message_id: message_id.clone(),
                delta: delta.clone(),
                replay_required: *replay_required,
            },
        )),
        EventFrame::AssistantReasoningCompleted {
            turn_id,
            message_id,
            replay_required,
        } => Some((
            turn_id.clone(),
            JournalEntry::ReasoningBlock {
                message_id: message_id.clone(),
                content: String::new(),
                replay_required: *replay_required,
            },
        )),
        EventFrame::ToolCallStarted {
            turn_id,
            call_id,
            tool_name,
        } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallRequested {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                arguments: serde_json::Value::Null,
            },
        )),
        EventFrame::ToolCallArgumentsDelta {
            turn_id,
            call_id,
            delta,
        } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallArgumentsDelta {
                call_id: call_id.clone(),
                delta: delta.clone(),
            },
        )),
        EventFrame::ToolCallApproved { turn_id, call_id } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallApproved {
                call_id: call_id.clone(),
            },
        )),
        EventFrame::ToolCallRejected { turn_id, call_id } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallRejected {
                call_id: call_id.clone(),
            },
        )),
        EventFrame::ToolCallCompleted {
            turn_id,
            call_id,
            tool_name: _,
            success,
            output_preview,
        } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallCompleted {
                call_id: call_id.clone(),
                tool_name: String::new(),
                success: *success,
                output: output_preview.clone(),
                output_preview: output_preview.clone(),
            },
        )),
        EventFrame::ToolCallFailed {
            turn_id,
            call_id,
            error,
        } => Some((
            turn_id.clone(),
            JournalEntry::ToolCallFailed {
                call_id: call_id.clone(),
                error: error.clone(),
            },
        )),
        EventFrame::ContextCompactionStarted { turn_id } => {
            Some((turn_id.clone(), JournalEntry::ContextCompactionStarted))
        }
        EventFrame::ContextCompactionCompleted { turn_id } => {
            Some((turn_id.clone(), JournalEntry::ContextCompactionCompleted))
        }
        EventFrame::SubagentSpawned {
            parent_turn_id,
            subagent_id,
            task,
        } => Some((
            parent_turn_id.clone(),
            JournalEntry::SubagentSpawned {
                parent_turn_id: parent_turn_id.clone(),
                subagent_id: subagent_id.clone(),
                task: task.clone(),
            },
        )),
        EventFrame::SubagentCompleted {
            parent_turn_id,
            subagent_id,
            result_summary,
        } => Some((
            parent_turn_id.clone(),
            JournalEntry::SubagentCompleted {
                parent_turn_id: parent_turn_id.clone(),
                subagent_id: subagent_id.clone(),
                result_summary: result_summary.clone(),
            },
        )),
        EventFrame::SubagentFailed {
            parent_turn_id,
            subagent_id,
            error,
        } => Some((
            parent_turn_id.clone(),
            JournalEntry::SubagentFailed {
                parent_turn_id: parent_turn_id.clone(),
                subagent_id: subagent_id.clone(),
                error: error.clone(),
            },
        )),
        EventFrame::ToolCallRequiresApproval { .. }
        | EventFrame::ThreadCreated { .. }
        | EventFrame::ThreadUpdated { .. }
        | EventFrame::ThreadArchived { .. }
        | EventFrame::TurnSteered { .. }
        | EventFrame::TurnInterrupted { .. }
        | EventFrame::RuntimeWarning { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deepomni_journal::InMemoryJournal;
    use deepomni_policy::{PermissionProfile, PolicyEngine};
    use deepomni_protocol::ToolOutput;
    use deepomni_tools::{ToolHandler, ToolInvocation, ToolRegistry};
    use std::sync::Arc;

    /// Simple test tool.
    struct TestReadTool;
    #[async_trait::async_trait]
    impl ToolHandler for TestReadTool {
        fn name(&self) -> &str {
            "read_file"
        }
        async fn handle(&self, _: ToolInvocation) -> Result<ToolOutput, deepomni_tools::ToolError> {
            Ok(ToolOutput::Function {
                body: Some(serde_json::json!({"content": "hello world"})),
                success: true,
            })
        }
    }

    #[tokio::test]
    async fn test_turn_runner_construction() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TestReadTool)).await;
        let policy = Arc::new(PolicyEngine::new(
            AgentMode::Agent,
            PermissionProfile::new(),
        ));
        let journal = Arc::new(InMemoryJournal::new());

        let runner = TurnRunner::new(
            Arc::new(registry),
            policy,
            journal,
            Arc::new(deepomni_trace::NoopTraceWriter),
        );
        // Construction succeeded.
        let _ = runner;
    }

    #[test]
    fn test_tool_output_preview() {
        let output = ToolOutput::Function {
            body: Some(serde_json::json!({"result": "ok"})),
            success: true,
        };
        let preview = tool_output_preview(&output);
        assert!(preview.contains("ok"));
    }

    #[test]
    fn request_compiler_new_turn_appends_user_input() {
        use deepomni_engine::{CompileConfig, RequestCompiler};
        let config = CompileConfig {
            system_prompt: Some("system"),
            fragments: &[],
            tools: vec![],
            max_output_tokens: Some(100),
            model: "mock-model",
            reasoning_to_replay: None,
        };

        let compiled = RequestCompiler::compile_new_turn(&config, &[], "hello");
        let messages = &compiled.request.messages;

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[1].role, MessageRole::User);
        assert_eq!(messages[1].content.as_deref(), Some("hello"));
    }

    #[test]
    fn request_compiler_tool_continuation_does_not_append_fake_user() {
        use deepomni_engine::{CompileConfig, RequestCompiler};
        let config = CompileConfig {
            system_prompt: Some("system"),
            fragments: &[],
            tools: vec![],
            max_output_tokens: Some(100),
            model: "mock-model",
            reasoning_to_replay: None,
        };
        let transcript = vec![ModelMessage {
            role: MessageRole::Tool,
            content: Some("tool output".into()),
            tool_calls: vec![],
            tool_call_id: Some("call-1".into()),
            reasoning_content: None,
        }];

        let compiled = RequestCompiler::compile_tool_continuation(&config, &transcript);
        let messages = &compiled.request.messages;

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::System);
        assert_eq!(messages[1].role, MessageRole::Tool);
        assert_eq!(messages[1].content.as_deref(), Some("tool output"));
    }
}
