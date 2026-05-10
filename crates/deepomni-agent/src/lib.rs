//! # DeepOmni Agent
//!
//! Agent loop and turn runner. Executes the core turn sequence:
//! input → context → provider → streaming → tool detection → orchestration
//! → approval → execution → continuation → completion.
//!
//! Implements DeepSeek reasoning handling and sub-agent control boundary.
//! PRD §7.4, §8.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use deepomni_context::{ContextFragment, FragmentRole, assemble_context};
use deepomni_events::EventBus;
use deepomni_model_provider::{
    MessageRole, ModelDelta, ModelMessage, ModelProvider, ModelRequest,
    ModelToolCall, ReasoningReplay,
};
use deepomni_policy::{AgentMode, PolicyEngine};
use deepomni_protocol::EventFrame;
use deepomni_protocol::id::{MessageId, SubagentId, ThreadId, ToolCallId, TurnId};
use deepomni_tools::{ToolInvocation, ToolRegistry};

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

/// The core turn runner.
///
/// Each turn follows PRD §8 sequence: receive input → persist →
/// build capability snapshot → assemble context → stream →
/// detect tools → orchestrate → approve → execute → continue → complete.
pub struct TurnRunner {
    tool_registry: Arc<ToolRegistry>,
    #[allow(dead_code)]
    policy_engine: Arc<PolicyEngine>,
    event_bus: Arc<EventBus>,
}

impl TurnRunner {
    pub fn new(
        tool_registry: Arc<ToolRegistry>,
        policy_engine: Arc<PolicyEngine>,
        event_bus: Arc<EventBus>,
    ) -> Self {
        Self { tool_registry, policy_engine, event_bus }
    }

    /// Run a single turn: send user input to the model, stream output,
    /// handle tool calls, and return the final result.
    pub async fn run_turn(
        &self,
        request: RunTurnRequest,
    ) -> Result<TurnResult, TurnError> {
        let RunTurnRequest {
            thread_id, turn_id, user_input, config, provider,
            conversation_history, reasoning_to_replay, item_sink,
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
        let fragments = Self::build_context_fragments(&config, &user_input, &conversation_history, &tool_specs);
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
            self.emit(thread_id.clone(), EventFrame::ContextCompactionStarted {
                turn_id: turn_id.clone(),
            })
            .await;
            self.emit(thread_id.clone(), EventFrame::ContextCompactionCompleted {
                turn_id: turn_id.clone(),
            })
            .await;
        }

        // 4. Build initial model request using the assembled context.
        let request = ModelRequest {
            model: ctx.model().to_string(),
            messages: Self::build_request_messages_from_assembled(
                &config,
                &assembled.messages,
                &conversation_history,
                &user_input,
            ),
            tools: tool_specs.clone(),
            max_output_tokens: ctx.config.max_output_tokens,
            temperature: None,
            system_prompt: ctx.config.system_prompt.clone(),
            replay_reasoning: reasoning_to_replay,
        };

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

        loop {
            iteration += 1;
            if iteration > MAX_ITERATIONS {
                break;
            }

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
                        self.emit(thread_id.clone(), EventFrame::AssistantMessageDelta {
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            delta: text,
                        }).await;
                    }
                    Some(ModelDelta::Reasoning { delta, replay_required }) => {
                        if current_reasoning.is_none() {
                            current_reasoning = Some(ReasoningState {
                                content: String::new(),
                                replay_required,
                            });
                        }
                        if let Some(ref mut rs) = current_reasoning {
                            rs.content.push_str(&delta);
                        }
                        self.emit(thread_id.clone(), EventFrame::AssistantReasoningDelta {
                            turn_id: turn_id.clone(),
                            message_id: message_id.clone(),
                            delta,
                            replay_required,
                            visibility: deepomni_protocol::event::ReasoningVisibility::HostRenderable,
                        }).await;
                    }
                    Some(ModelDelta::ToolCallStart { call_id, tool_name }) => {
                        active_tool_call = Some(ToolCallState {
                            call_id: ToolCallId::from_string(&call_id),
                            tool_name,
                            arguments: String::new(),
                        });
                        self.emit(thread_id.clone(), EventFrame::ToolCallStarted {
                            turn_id: turn_id.clone(),
                            call_id: ToolCallId::from_string(&call_id),
                            tool_name: active_tool_call.as_ref().unwrap().tool_name.clone(),
                        }).await;
                    }
                    Some(ModelDelta::ToolCallArgumentsDelta { call_id, delta }) => {
                        if let Some(ref mut tc) = active_tool_call
                            && tc.call_id.as_str() == call_id {
                                tc.arguments.push_str(&delta);
                            }
                        self.emit(thread_id.clone(), EventFrame::ToolCallArgumentsDelta {
                            turn_id: turn_id.clone(),
                            call_id: ToolCallId::from_string(&call_id),
                            delta,
                        }).await;
                    }
                    Some(ModelDelta::ToolCallComplete { call_id: _, tool_name: _, arguments }) => {
                        if let Some(tc) = active_tool_call.take() {
                            had_tool_call_this_iteration = true;

                            if let Some(ref rs) = current_reasoning {
                                ctx.record(deepomni_protocol::TurnItem::ReasoningBlock {
                                    content: rs.content.clone(),
                                    replay_required: rs.replay_required,
                                });
                                self.emit(thread_id.clone(), EventFrame::AssistantReasoningCompleted {
                                    turn_id: turn_id.clone(),
                                    message_id: message_id.clone(),
                                    replay_required: rs.replay_required,
                                }).await;
                            }

                            let decision = self.tool_registry.get(&tc.tool_name)
                                .map(|h| deepomni_tools::ToolOrchestrator::evaluate(
                                    h.as_ref(),
                                    &ctx.config.agent_mode,
                                ))
                                .unwrap_or(deepomni_tools::OrchestratorDecision::Forbidden {
                                    reason: format!("unknown tool: {}", tc.tool_name),
                                });

                            match decision {
                                deepomni_tools::OrchestratorDecision::Proceed { .. } => {
                                    let result = self.execute_tool(&tc, &arguments, &ctx).await?;
                                    ctx.record(deepomni_protocol::TurnItem::ToolCall {
                                        call_id: tc.call_id.clone(),
                                        tool_name: tc.tool_name.clone(),
                                        arguments: arguments.clone(),
                                    });
                                    ctx.record(deepomni_protocol::TurnItem::ToolResult {
                                        call_id: tc.call_id.clone(),
                                        tool_name: tc.tool_name.clone(),
                                        output_preview: result.preview.clone(),
                                        success: result.success,
                                    });
                                    self.emit(thread_id.clone(), EventFrame::ToolCallCompleted {
                                        turn_id: turn_id.clone(),
                                        call_id: tc.call_id.clone(),
                                        success: result.success,
                                        output_preview: result.preview.clone(),
                                    }).await;

                                    iteration_tool_results.push(ModelMessage {
                                        role: MessageRole::Tool,
                                        content: Some(result.content.clone()),
                                        tool_calls: vec![],
                                        tool_call_id: Some(tc.call_id.to_string()),
                                        reasoning_content: None,
                                    });

                                    pending_tool_calls.push(ToolCallState {
                                        call_id: tc.call_id.clone(),
                                        tool_name: tc.tool_name.clone(),
                                        arguments: tc.arguments.clone(),
                                    });
                                    iteration_tool_calls.push(ModelToolCall {
                                        call_id: tc.call_id.to_string(),
                                        tool_name: tc.tool_name.clone(),
                                        arguments: serde_json::from_str(&tc.arguments).unwrap_or_default(),
                                    });
                                }
                                deepomni_tools::OrchestratorDecision::NeedsApproval { reason, approval_id } => {
                                    // Persist the pending tool call as a turn item before pausing.
                                    ctx.record(deepomni_protocol::TurnItem::ToolCall {
                                        call_id: tc.call_id.clone(),
                                        tool_name: tc.tool_name.clone(),
                                        arguments: arguments.clone(),
                                    });
                                    self.emit(thread_id.clone(), EventFrame::ToolCallRequiresApproval {
                                        turn_id: turn_id.clone(),
                                        call_id: tc.call_id.clone(),
                                        tool_name: tc.tool_name.clone(),
                                        reason: reason.clone(),
                                    }).await;
                                    return Ok(TurnResult::NeedsApproval {
                                        turn_id,
                                        call_id: tc.call_id,
                                        tool_name: tc.tool_name,
                                        arguments,
                                        approval_id,
                                    });
                                }
                                deepomni_tools::OrchestratorDecision::Forbidden { reason } => {
                                    self.emit(thread_id.clone(), EventFrame::ToolCallFailed {
                                        turn_id: turn_id.clone(),
                                        call_id: tc.call_id.clone(),
                                        error: reason,
                                    }).await;
                                }
                            }
                        }
                    }
                }
            }

            // After stream ends, if tools were executed this iteration,
            // append to transcript and continue.
            if had_tool_call_this_iteration && !iteration_tool_results.is_empty() {
                // Append exactly this round's assistant tool_calls + matching tool results.
                let assistant_tool_calls = std::mem::take(&mut iteration_tool_calls);
                transcript.push(ModelMessage {
                    role: MessageRole::Assistant,
                    content: if final_answer.is_empty() { None } else { Some(final_answer.clone()) },
                    tool_calls: assistant_tool_calls,
                    tool_call_id: None,
                    reasoning_content: current_reasoning.as_ref().map(|r| r.content.clone()),
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
        }

        // 6. Complete the turn.
        if !final_answer.is_empty() {
            self.emit(thread_id.clone(), EventFrame::AssistantMessageCompleted {
                turn_id: turn_id.clone(),
                message_id,
            })
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
            thread_id, turn_id, call_id, tool_name, arguments,
            config, provider, tool_result_content, transcript,
            reasoning_to_replay, item_sink,
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
        self.emit(thread_id.clone(), EventFrame::ToolCallApproved {
            turn_id: turn_id.clone(),
            call_id: call_id.clone(),
        })
        .await;

        // Execute the approved tool.
        let tc = ToolCallState {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            arguments: serde_json::to_string(&arguments).unwrap_or_default(),
        };
        let result = self.execute_tool(&tc, &arguments, &ctx).await?;

        ctx.record(deepomni_protocol::TurnItem::ToolResult {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            output_preview: result.preview.clone(),
            success: result.success,
        });
        ctx.record(deepomni_protocol::TurnItem::ApprovalDecision {
            call_id: call_id.clone(),
            approved: true,
        });

        self.emit(thread_id.clone(), EventFrame::ToolCallCompleted {
            turn_id: turn_id.clone(),
            call_id: call_id.clone(),
            success: result.success,
            output_preview: result.preview.clone(),
        })
        .await;

        // Continue with the full transcript + tool result, not a fake user input.
        let mut continuation_transcript = transcript;
        continuation_transcript.push(ModelMessage {
            role: MessageRole::Tool,
            content: Some(result.content.clone()),
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
            item_sink: item_sink_for_continuation,
        })
        .await
    }

    /// Execute a tool and return the result.
    async fn execute_tool(
        &self,
        tc: &ToolCallState,
        arguments: &serde_json::Value,
        ctx: &TurnExecutionContext,
    ) -> Result<ToolExecutionResult, TurnError> {
        let handler = self.tool_registry.get(&tc.tool_name).ok_or_else(|| {
            TurnError::UnknownTool(tc.tool_name.clone())
        })?;

        let is_mutating = handler.is_mutating();
        let invocation = ToolInvocation {
            call_id: tc.call_id.clone(),
            tool_name: tc.tool_name.clone(),
            payload: deepomni_protocol::tool::ToolPayload::Function {
                arguments: serde_json::to_string(arguments).unwrap_or_default(),
            },
            timeout: Some(Duration::from_secs(60)),
            allow_mutating: is_mutating,
            workspace: Some(ctx.workspace().to_string()),
        };

        match self.tool_registry.dispatch(invocation).await {
            Ok(output) => {
                let preview = tool_output_preview(&output);
                Ok(ToolExecutionResult {
                    success: true,
                    content: preview.clone(),
                    preview: Some(preview),
                })
            }
            Err(e) => Ok(ToolExecutionResult {
                success: false,
                content: format!("{e}"),
                preview: Some(format!("error: {e}")),
            }),
        }
    }

    /// Emit an event through the event bus. Seq is assigned by the
    /// persistence layer; the agent passes a placeholder seq that is
    /// replaced when the runtime's state-backed emit path takes over.
    async fn emit(&self, thread_id: ThreadId, event: EventFrame) {
        let _ = self.event_bus.emit(thread_id, 0, event).await;
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

    /// Build model request messages from the assembled context (uses budget-aware fragments).
    fn build_request_messages_from_assembled(
        config: &TurnConfig,
        fragments: &[ContextFragment],
        history: &[ModelMessage],
        user_input: &str,
    ) -> Vec<ModelMessage> {
        let mut messages = Vec::new();

        // System prompt FIRST (before transcript).
        let system_content = fragments
            .iter()
            .find_map(|f| match f {
                ContextFragment::System { content } => Some(content.clone()),
                _ => None,
            })
            .or_else(|| config.system_prompt.clone());

        if let Some(sys) = system_content {
            messages.push(ModelMessage {
                role: MessageRole::System,
                content: Some(sys),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }

        // Conversation messages + tagged developer fragments from assembled context.
        for fragment in fragments.iter() {
            match fragment {
                ContextFragment::ConversationMessage { role, content, .. } => {
                    messages.push(ModelMessage {
                        role: match role.as_str() {
                            "user" => MessageRole::User,
                            "assistant" => MessageRole::Assistant,
                            _ => MessageRole::User,
                        },
                        content: Some(content.clone()),
                        tool_calls: vec![],
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
                // Developer-role fragments are injected as marked developer messages
                // in conversation history, not in the system prompt. Pattern from
                // Codex `ContextualUserFragment` / Claude Code developer-message injection.
                f if f.role() == FragmentRole::Developer => {
                    let tag = f.marker_tag().unwrap_or("context");
                    let content = match f {
                        ContextFragment::ToolSchemas { content }
                        | ContextFragment::SkillInstruction { content, .. }
                        | ContextFragment::PluginCapability { content, .. }
                        | ContextFragment::Environment { content } => content.clone(),
                        _ => continue,
                    };
                    messages.push(ModelMessage {
                        role: MessageRole::User,
                        content: Some(format!("<{tag}>\n{content}\n</{tag}>")),
                        tool_calls: vec![],
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
                _ => {}
            }
        }

        // Append transcript AFTER system/developer context.
        messages.extend_from_slice(history);

        // Append user input only for new turns, not for continuations
        // (continuations pass the transcript as history with their own messages).
        if !history.is_empty() {
            // This is a continuation — don't inject a fake user message.
        } else {
            messages.push(ModelMessage {
                role: MessageRole::User,
                content: Some(user_input.to_string()),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            });
        }

        messages
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

        self.emit(parent_thread_id.clone(), EventFrame::SubagentSpawned {
            parent_turn_id: parent_turn_id.clone(),
            subagent_id: subagent_id.clone(),
            task: task.clone(),
        })
        .await;

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
                item_sink: None,
            })
            .await;

        match result {
            Ok(TurnResult::Completed { answer, .. }) => {
                self.emit(parent_thread_id, EventFrame::SubagentCompleted {
                    parent_turn_id,
                    subagent_id: subagent_id.clone(),
                    result_summary: answer.clone(),
                })
                .await;
                Ok(SubagentResult {
                    subagent_id,
                    answer,
                })
            }
            Ok(other) => {
                self.emit(parent_thread_id, EventFrame::SubagentFailed {
                    parent_turn_id,
                    subagent_id: subagent_id.clone(),
                    error: format!("unexpected turn result: {other:?}"),
                })
                .await;
                Ok(SubagentResult {
                    subagent_id,
                    answer: "sub-agent did not complete normally".into(),
                })
            }
            Err(e) => {
                self.emit(parent_thread_id, EventFrame::SubagentFailed {
                    parent_turn_id,
                    subagent_id: subagent_id.clone(),
                    error: format!("{e}"),
                })
                .await;
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

#[derive(Debug)]
struct ToolExecutionResult {
    success: bool,
    content: String,
    preview: Option<String>,
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
fn tool_output_preview(output: &deepomni_protocol::tool::ToolOutput) -> String {
    match output {
        deepomni_protocol::tool::ToolOutput::Function { body, .. } => {
            body.as_ref()
                .map(|v| {
                    let s = serde_json::to_string(v).unwrap_or_default();
                    if s.len() > 200 {
                        format!("{}...", &s[..200])
                    } else {
                        s
                    }
                })
                .unwrap_or_else(|| "ok".into())
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use deepomni_policy::{PermissionProfile, PolicyEngine};
    use deepomni_tools::{ToolHandler, ToolRegistry};
    use deepomni_events::EventBus;
    use deepomni_protocol::ToolOutput;

    /// Simple test tool.
    struct TestReadTool;
    #[async_trait::async_trait]
    impl ToolHandler for TestReadTool {
        fn name(&self) -> &str { "read_file" }
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
        let policy = Arc::new(PolicyEngine::new(AgentMode::Agent, PermissionProfile::new()));
        let events = Arc::new(EventBus::new());

        let runner = TurnRunner::new(
            Arc::new(registry),
            policy,
            events,
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
}
