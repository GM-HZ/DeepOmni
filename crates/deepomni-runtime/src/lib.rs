//! # DeepOmni Runtime
//!
//! Main host-agnostic runtime facade. Provides the `Runtime` struct and
//! `RuntimeBuilder` for creating, managing, and interacting with agent
//! threads, turns, approvals, and event subscriptions.
//!
//! PRD §7.3, §10.1

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{RwLock, Mutex};

use deepomni_agent::{TurnConfig, TurnError, TurnResult, TurnRunner};
use deepomni_config::{ConfigStore, ResolvedConfig};
use deepomni_events::EventBus;
use deepomni_model_provider::{ModelProvider, ProviderRegistry};
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::{
    CreateThreadRequest, CreateTurnRequest, EventFrame,
    Thread, ThreadStatus, Turn, TurnStatus, SessionSource,
};
use deepomni_protocol::id::{ThreadId, ToolCallId, TurnId};
use deepomni_state::StateStore;
use deepomni_tools::ToolRegistry;

/// The main runtime facade.
///
/// All public handles are cheap `Arc` clones.
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

/// Serializable pending approval record stored in the tool_calls table.
/// Does NOT hold Arc<dyn ModelProvider> — on resume, the provider is
/// resolved from the registry by model name.
struct PendingApproval {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub approval_id: String,
    pub model: String,
    pub workspace: String,
    pub config: deepomni_agent::TurnConfig,
}

struct RuntimeInner {
    config: ResolvedConfig,
    state: Arc<StateStore>,
    event_bus: Arc<EventBus>,
    tool_registry: Arc<ToolRegistry>,
    policy_engine: Arc<PolicyEngine>,
    provider_registry: RwLock<ProviderRegistry>,
    turn_runner: TurnRunner,
    skill_manager: RwLock<deepomni_skills::SkillManager>,
    plugin_manager: RwLock<deepomni_plugin::PluginManager>,
    hook_dispatcher: Arc<deepomni_hooks::HookDispatcher>,
    active_threads: Arc<RwLock<HashMap<ThreadId, ActiveThread>>>,
    name_counter: Mutex<u64>,
    /// Pending approvals keyed by approval_id.
    pending_approvals: RwLock<HashMap<String, PendingApproval>>,
    /// Active sub-agents tracked by subagent_id.
    subagent_registry: RwLock<HashMap<String, SubagentState>>,
}

/// Runtime state for a spawned sub-agent.
struct SubagentState {
    pub subagent_id: String,
    pub parent_thread_id: ThreadId,
    pub parent_turn_id: TurnId,
    pub task: String,
    pub status: SubagentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SubagentStatus {
    Running,
    Completed,
    Failed,
}

/// An active thread in the runtime.
#[derive(Debug, Clone)]
struct ActiveThread {
    pub thread: Thread,
    pub status: ThreadStatus,
    pub active_turn_id: Option<TurnId>,
}

// ── RuntimeBuilder ──

/// Builder for constructing a Runtime with all required subsystems.
pub struct RuntimeBuilder {
    workspace: Option<std::path::PathBuf>,
    config: Option<ResolvedConfig>,
    state_path: Option<std::path::PathBuf>,
}

impl RuntimeBuilder {
    pub fn new() -> Self {
        Self {
            workspace: None,
            config: None,
            state_path: None,
        }
    }

    pub fn workspace(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.workspace = Some(path.into());
        self
    }

    pub fn config(mut self, config: ResolvedConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn state_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.state_path = Some(path.into());
        self
    }

    pub async fn build(self) -> Result<Runtime, RuntimeError> {
        // Load config.
        let config = match self.config {
            Some(c) => c,
            None => {
                let store = ConfigStore::load_default()
                    .map_err(|e| RuntimeError::Config(format!("{e}")))?;
                store
                    .resolve(&deepomni_config::CliOverrides::default(), self.workspace.as_deref())
                    .map_err(|e| RuntimeError::Config(format!("{e}")))?
            }
        };

        // Open state.
        let state = Arc::new(StateStore::open(self.state_path)
            .map_err(|e| RuntimeError::State(format!("{e}")))?);

        // Initialize subsystems.
        // Wire EventBus persistence: events are durably written to StateStore
        // before broadcast so they survive restarts and can be replayed.
        let persist_state = state.clone();
        let event_bus = Arc::new(EventBus::new().with_persistence(move |tid, _seq, event| {
            let turn_id = extract_turn_id(event);
            persist_state.append_event(tid.as_str(), turn_id, event)
                .map_err(|e| format!("{e}"))
        }));
        let mut tool_registry = ToolRegistry::new();
        // Register all built-in tools with the workspace path.
        let ws = self.workspace.as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".into());
        deepomni_tool_builtin::register_all(&mut tool_registry, &ws).await;
        let tool_registry = Arc::new(tool_registry);
        let mut provider_registry = ProviderRegistry::new();
        // Auto-register DeepSeek provider from config when an API key is available.
        if let Some(ref api_key) = config.api_key {
            use deepomni_provider_deepseek::DeepSeekProvider;
            let deepseek = Arc::new(
                DeepSeekProvider::new(api_key.clone())
                    .with_base_url(config.base_url.clone()),
            );
            provider_registry.register(deepseek);
        }
        let provider_registry = RwLock::new(provider_registry);
        let base_profile = PermissionProfile::new()
            .allow(deepomni_protocol::PermissionType::FilesystemRead)
            .allow(deepomni_protocol::PermissionType::FilesystemWrite);
        let policy_engine = Arc::new(PolicyEngine::new(AgentMode::Agent, base_profile));
        let turn_runner = TurnRunner::new(
            tool_registry.clone(),
            policy_engine.clone(),
            event_bus.clone(),
        );

        // Initialize skills from configured paths.
        let mut skill_roots = deepomni_skills::SkillRoots::new();
        for path in &config.skill_paths {
            skill_roots.add_root(path.clone());
        }
        let mut skill_manager = deepomni_skills::SkillManager::new(skill_roots);
        let _ = skill_manager.discover();

        // Initialize plugins from configured paths.
        let mut plugin_manager = deepomni_plugin::PluginManager::new(config.plugin_paths.clone());
        let _ = plugin_manager.discover();

        // Discover plugin-contributed skills.
        for plugin_path in plugin_manager.active_skill_paths() {
            for plugin in plugin_manager.list_enabled() {
                let _ = skill_manager.discover_plugin_skills(
                    &plugin_path,
                    &plugin.manifest.id,
                );
            }
        }

        // Register MCP tools from active plugins (deferred to server crate).

        let hook_dispatcher = Arc::new(deepomni_hooks::HookDispatcher::new());

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                config,
                state,
                event_bus,
                tool_registry,
                policy_engine,
                provider_registry,
                turn_runner,
                skill_manager: RwLock::new(skill_manager),
                plugin_manager: RwLock::new(plugin_manager),
                hook_dispatcher,
                active_threads: Arc::new(RwLock::new(HashMap::new())),
                name_counter: Mutex::new(0),
                pending_approvals: RwLock::new(HashMap::new()),
                subagent_registry: RwLock::new(HashMap::new()),
            }),
        })
    }
}

impl Default for RuntimeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Runtime API ──

impl Runtime {
    /// Return a reference to the resolved config.
    pub fn config(&self) -> &ResolvedConfig {
        &self.inner.config
    }

    /// Get a clone of the tool registry for registration.
    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        self.inner.tool_registry.clone()
    }

    /// Get a clone of the event bus for direct access.
    pub fn event_bus(&self) -> Arc<EventBus> {
        self.inner.event_bus.clone()
    }

    /// Register a model provider into the runtime.
    pub async fn register_provider(&self, provider: Arc<dyn ModelProvider>) {
        self.inner.provider_registry.write().await.register(provider);
    }

    /// Create a new thread.
    pub async fn create_thread(
        &self,
        request: CreateThreadRequest,
    ) -> Result<Thread, RuntimeError> {
        let thread_id = ThreadId::new();
        let now = current_timestamp();
        let cwd = request.workspace.display().to_string();

        // Generate a human-readable name.
        let counter = {
            let mut guard = self.inner.name_counter.lock().await;
            *guard += 1;
            *guard
        };
        let name = request.name.unwrap_or_else(|| format!("Thread {counter}"));

        let thread = Thread {
            id: thread_id.clone(),
            preview: String::new(),
            ephemeral: request.ephemeral,
            model_provider: self.inner.config.provider.as_str().to_string(),
            created_at: now,
            updated_at: now,
            status: ThreadStatus::Idle,
            path: Some(request.workspace.clone()),
            cwd: request.workspace,
            cli_version: "0.1.0".into(),
            source: SessionSource::Interactive,
            name: Some(name),
            sandbox_policy: request.sandbox,
            approval_mode: request.approval_policy,
            parent_thread_id: request.parent_thread_id,
        };

        // Persist.
        self.persist_thread(&thread)?;
        self.inner.active_threads.write().await.insert(
            thread_id.clone(),
            ActiveThread {
                thread: thread.clone(),
                status: ThreadStatus::Idle,
                active_turn_id: None,
            },
        );

        // Emit event.
        self.emit_event(&thread_id, EventFrame::ThreadCreated {
            thread_id: thread_id.clone(),
            workspace: cwd,
        })
        .await;

        Ok(thread)
    }

    /// Start and execute a turn on a thread.
    ///
    /// Creates the turn, resolves the model provider, emits `turn.started`,
    /// runs the agent loop through `TurnRunner`, and emits `turn.completed`
    /// or `turn.failed`.
    pub async fn submit_turn(
        &self,
        thread_id: ThreadId,
        request: CreateTurnRequest,
    ) -> Result<Turn, RuntimeError> {
        let turn_id = TurnId::new();
        let now = current_timestamp();

        // Check thread exists, enforce single active turn, capture workspace.
        let ws = {
            let mut threads = self.inner.active_threads.write().await;
            let active = threads.get_mut(&thread_id).ok_or_else(|| {
                RuntimeError::ThreadNotFound(thread_id.clone())
            })?;
            if active.active_turn_id.is_some() {
                return Err(RuntimeError::InvalidTransition(
                    "thread already has an active turn".into(),
                ));
            }
            active.active_turn_id = Some(turn_id.clone());
            active.thread.cwd.display().to_string()
        };

        let model = request.model.clone()
            .or_else(|| Some(self.inner.config.model.clone()))
            .unwrap_or_else(|| "deepseek-v4-pro".into());

        // Helper to clear active turn slot synchronously before any early exit.
        async fn clear_active_turn(
            threads: &RwLock<HashMap<ThreadId, ActiveThread>>,
            tid: &ThreadId,
            tuid: &TurnId,
        ) {
            let mut guard = threads.write().await;
            if let Some(active) = guard.get_mut(tid) {
                if active.active_turn_id.as_ref() == Some(tuid) {
                    active.active_turn_id = None;
                }
            }
        }

        let turn = Turn {
            id: turn_id.clone(),
            thread_id: thread_id.clone(),
            status: TurnStatus::Started,
            user_input: request.input.clone(),
            created_at: now,
            completed_at: None,
            model: Some(model.clone()),
            model_provider: Some(self.inner.config.provider.as_str().to_string()),
            parent_turn_id: request.parent_turn_id,
            parent_thread_id: None,
            subagent_id: request.subagent_id,
        };

        // Persist the turn start. On failure, clear active slot before returning.
        if let Err(e) = self.persist_turn(&turn) {
            clear_active_turn(&self.inner.active_threads, &thread_id, &turn_id).await;
            return Err(e);
        }

        // Fire UserPromptSubmitted hooks and collect injected context.
        let hook_context = self.inner.hook_dispatcher.dispatch_and_collect_context(
            deepomni_hooks::HookPoint::UserPromptSubmitted,
            deepomni_hooks::HookContext {
                hook_point: deepomni_hooks::HookPoint::UserPromptSubmitted,
                thread_id: Some(thread_id.clone()),
                turn_id: Some(turn_id.clone()),
                tool_name: None,
                tool_call_id: None,
                data: Some(request.input.clone()),
                approved: None,
            },
        )
        .await;

        // Emit turn.started event.
        self.emit_event(&thread_id, EventFrame::TurnStarted {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            user_input: request.input.clone(),
        })
        .await;

        // Resolve provider.
        let provider_arc = match {
            let registry = self.inner.provider_registry.read().await;
            registry.resolve(&model).map(|(_, p)| p.clone())
        } {
            Some(provider) => provider,
            None => {
                clear_active_turn(&self.inner.active_threads, &thread_id, &turn_id).await;
                return Err(RuntimeError::NotReady(format!("no provider for model '{model}'")));
            }
        };

        // Inject active skills and hook context into the system prompt.
        let skill_context = self.inner.skill_manager.read().await.render_for_context(10_000);
        let base_prompt = String::new(); // System prompt is built in the agent from context fragments.
        let hook_augmented = if !hook_context.is_empty() {
            format!("{base_prompt}\n\n<system-reminder>\n{hook_context}\n</system-reminder>")
        } else {
            base_prompt
        };
        let augmented_prompt = if !skill_context.is_empty() {
            format!("{hook_augmented}\n\n<available-skills>\n{skill_context}\n</available-skills>")
        } else {
            hook_augmented
        };

        // Build system prompt and compute cache hash for provider.
        let prompt = deepomni_prompt::PromptBuilder::new(
            augmented_prompt.clone(),
        ).build();
        tracing::debug!(
            prompt_hash = %prompt.static_hash,
            prompt_len = prompt.content.len(),
            "system prompt assembled"
        );

        // Build turn config. Clone values needed later (e.g., for PendingApproval).
        let model_pending = model.clone();
        let ws_pending = ws.clone();
        let config = TurnConfig {
            model,
            max_output_tokens: request.max_token_budget,
            token_budget: 100_000,
            turn_timeout: None,
            agent_mode: self.inner.policy_engine.mode(),
            system_prompt: if augmented_prompt.is_empty() { None } else { Some(augmented_prompt) },
            workspace: ws,
        };

        // Build turn item sink that persists to durable state.
        let item_state = self.inner.state.clone();
        let item_tid = thread_id.to_string();
        let item_turn_id = turn_id.to_string();
        let item_sink: Arc<dyn Fn(deepomni_protocol::TurnItem) + Send + Sync> =
            Arc::new(move |item: deepomni_protocol::TurnItem| {
                let _ = item_state.insert_turn_item(&item_tid, &item_turn_id, &item);
            });

        // Run the agent loop.
        let result = self.inner.turn_runner.run_turn(
            deepomni_agent::RunTurnRequest {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
                user_input: request.input,
                config,
                provider: provider_arc,
                conversation_history: vec![],
                reasoning_to_replay: None,
                item_sink: Some(item_sink),
            },
        )
        .await;

        // Clear active turn slot on any terminal state.
        {
            let mut threads = self.inner.active_threads.write().await;
            if let Some(active) = threads.get_mut(&thread_id) {
                active.active_turn_id = None;
            }
        }

        match result {
            Ok(TurnResult::Completed { .. }) => {
                let completed_at = current_timestamp();
                self.inner.state.update_turn_status(
                    turn_id.as_str(),
                    "completed",
                    Some(completed_at),
                ).map_err(|e| RuntimeError::State(format!("{e}")))?;

                self.must_emit_event(&thread_id, EventFrame::TurnCompleted {
                    turn_id: turn_id.clone(),
                })
                .await?;

                Ok(Turn {
                    status: TurnStatus::Completed,
                    completed_at: Some(completed_at),
                    ..turn
                })
            }
            Ok(TurnResult::NeedsApproval { call_id, tool_name, arguments, approval_id, .. }) => {
                // Persist to durable state so approval survives restart.
                let durable_rec = deepomni_state::PendingApprovalRecord {
                    approval_id: approval_id.clone(),
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    call_id: call_id.to_string(),
                    tool_name: tool_name.clone(),
                    arguments_json: serde_json::to_string(&arguments).unwrap_or_default(),
                    model: model_pending.clone(),
                    workspace: ws_pending.clone(),
                    config_json: serde_json::to_string(&TurnConfig {
                        model: model_pending.clone(), max_output_tokens: request.max_token_budget,
                        token_budget: 100_000, turn_timeout: None,
                        agent_mode: self.inner.policy_engine.mode(), system_prompt: None,
                        workspace: ws_pending.clone(),
                    }).unwrap_or_default(),
                    status: "pending".to_string(),
                    created_at: current_timestamp(),
                };
                self.inner.state.insert_pending_approval(&durable_rec)
                    .map_err(|e| RuntimeError::State(format!("failed to persist pending approval: {e}")))?;

                // Also keep in memory for fast lookup during this session.
                self.inner.pending_approvals.write().await.insert(
                    approval_id.clone(),
                    PendingApproval {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                        call_id,
                        tool_name,
                        arguments,
                        approval_id: approval_id.clone(),
                        model: model_pending.clone(),
                        workspace: ws_pending.clone(),
                        config: TurnConfig {
                            model: model_pending,
                            max_output_tokens: request.max_token_budget,
                            token_budget: 100_000,
                            turn_timeout: None,
                            agent_mode: self.inner.policy_engine.mode(),
                            system_prompt: None,
                            workspace: ws_pending,
                        },
                    },
                );
                // Re-set active_turn_id since the turn is still pending.
                {
                    let mut threads = self.inner.active_threads.write().await;
                    if let Some(active) = threads.get_mut(&thread_id) {
                        active.active_turn_id = Some(turn_id.clone());
                    }
                }
                // Host must call approve_tool to continue.
                // Re-set active_turn_id since the turn is still pending.
                {
                    let mut threads = self.inner.active_threads.write().await;
                    if let Some(active) = threads.get_mut(&thread_id) {
                        active.active_turn_id = Some(turn_id.clone());
                    }
                }
                Ok(Turn {
                    status: TurnStatus::WaitingForApproval,
                    ..turn
                })
            }
            Err(e) => {
                self.must_emit_event(&thread_id, EventFrame::TurnFailed {
                    turn_id: turn_id.clone(),
                    error: format!("{e}"),
                })
                .await?;

                Ok(Turn {
                    status: TurnStatus::Failed,
                    ..turn
                })
            }
        }
    }

    /// List threads from durable state.
    pub fn list_threads(&self) -> Result<Vec<deepomni_state::ThreadRecord>, RuntimeError> {
        self.inner
            .state
            .list_threads(false, 100)
            .map_err(|e| RuntimeError::State(format!("{e}")))
    }

    /// Get a thread by ID from durable state.
    pub fn get_thread(&self, thread_id: &str) -> Result<deepomni_state::ThreadRecord, RuntimeError> {
        self.inner
            .state
            .get_thread(thread_id)
            .map_err(|e| RuntimeError::State(format!("{e}")))?
            .ok_or_else(|| RuntimeError::ThreadNotFound(ThreadId::from_string(thread_id)))
    }

    /// Subscribe to live events for a thread.
    pub async fn subscribe(
        &self,
        thread_id: ThreadId,
    ) -> deepomni_events::EventSubscriber {
        self.inner.event_bus.subscribe(thread_id).await
    }

    /// Resolve a pending approval: try in-memory cache first, fall back to
    /// durable state on miss (survives process restart). Rebuilds from the
    /// pending_approvals table record including config/model/workspace/args.
    async fn resolve_pending_approval(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
    ) -> Result<PendingApproval, RuntimeError> {
        // 1. Try in-memory cache.
        {
            let mut approvals = self.inner.pending_approvals.write().await;
            let key = approvals.iter()
                .find(|(_, v)| &v.thread_id == thread_id && &v.turn_id == turn_id)
                .map(|(k, _)| k.clone());
            if let Some(k) = key {
                return approvals.remove(&k)
                    .ok_or_else(|| RuntimeError::NotReady("approval entry disappeared".into()));
            }
        }

        // 2. Fall back to durable state.
        let durable = self.inner.state.get_pending_approval_by_turn(
            thread_id.as_str(), turn_id.as_str(),
        ).map_err(|e| RuntimeError::State(format!("{e}")))?
        .ok_or_else(|| RuntimeError::NotReady("no pending approval for this turn".into()))?;

        let config: TurnConfig = serde_json::from_str(&durable.config_json)
            .unwrap_or_else(|_| TurnConfig {
                model: durable.model.clone(),
                max_output_tokens: None, token_budget: 100_000, turn_timeout: None,
                agent_mode: deepomni_policy::AgentMode::Agent,
                system_prompt: None, workspace: durable.workspace.clone(),
            });

        Ok(PendingApproval {
            thread_id: thread_id.clone(), turn_id: turn_id.clone(),
            call_id: ToolCallId::from_string(&durable.call_id),
            tool_name: durable.tool_name,
            arguments: serde_json::from_str(&durable.arguments_json).unwrap_or_default(),
            approval_id: durable.approval_id,
            model: durable.model, workspace: durable.workspace, config,
        })
    }

    /// Approve a pending tool call and resume the turn.
    pub async fn approve_tool(
        &self,
        thread_id: ThreadId,
        turn_id: TurnId,
    ) -> Result<Turn, RuntimeError> {
        let pending = self.resolve_pending_approval(&thread_id, &turn_id).await?;

        // Mark approved in durable state. Propagate failure so we don't
        // proceed with the tool if durable state is inconsistent.
        self.inner.state.update_pending_approval_status(&pending.approval_id, "approved")
            .map_err(|e| RuntimeError::State(format!("failed to update approval status: {e}")))?;

        let provider = {
            let registry = self.inner.provider_registry.read().await;
            registry.resolve(&pending.model)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| RuntimeError::NotReady(
                    format!("provider for model '{}' not available", pending.model)
                ))?
        };

        let transcript = self.build_transcript_from_items(&pending.turn_id);

        // Build item_sink so resumed tool results are durably journaled.
        let item_state = self.inner.state.clone();
        let item_tid = thread_id.to_string();
        let item_turn_id = turn_id.to_string();
        let item_sink: Arc<dyn Fn(deepomni_protocol::TurnItem) + Send + Sync> =
            Arc::new(move |item| { let _ = item_state.insert_turn_item(&item_tid, &item_turn_id, &item); });

        let result = self.inner.turn_runner.continue_after_approval(
            deepomni_agent::ResumeTurnRequest {
                thread_id: thread_id.clone(), turn_id: turn_id.clone(),
                call_id: pending.call_id.clone(), tool_name: pending.tool_name.clone(),
                arguments: pending.arguments.clone(), config: pending.config.clone(),
                provider, tool_result_content: String::new(), transcript,
                reasoning_to_replay: None, item_sink: Some(item_sink),
            },
        ).await;

        let final_status = match result {
            Ok(TurnResult::Completed { .. }) => {
                self.must_emit_event(&thread_id, EventFrame::TurnCompleted { turn_id: turn_id.clone() }).await?;
                TurnStatus::Completed
            }
            Ok(TurnResult::NeedsApproval { .. }) => TurnStatus::WaitingForApproval,
            Err(e) => {
                self.must_emit_event(&thread_id, EventFrame::TurnFailed { turn_id: turn_id.clone(), error: format!("{e}") }).await?;
                TurnStatus::Failed
            }
        };

        if final_status != TurnStatus::WaitingForApproval {
            let mut threads = self.inner.active_threads.write().await;
            if let Some(active) = threads.get_mut(&thread_id) { active.active_turn_id = None; }
        }

        self.inner.state.update_turn_status(turn_id.as_str(), &format!("{final_status:?}").to_lowercase(), Some(current_timestamp()))
            .map_err(|e| RuntimeError::State(format!("{e}")))?;

        Ok(Turn { id: turn_id, thread_id, status: final_status, user_input: String::new(), created_at: current_timestamp(), completed_at: Some(current_timestamp()), model: None, model_provider: None, parent_turn_id: None, parent_thread_id: None, subagent_id: None })
    }

    /// Reject a pending tool call and fail the turn.
    pub async fn reject_tool(
        &self,
        thread_id: ThreadId,
        turn_id: TurnId,
    ) -> Result<Turn, RuntimeError> {
        let pending = self.resolve_pending_approval(&thread_id, &turn_id).await?;

        // Mark rejected in durable state. Propagate failure.
        self.inner.state.update_pending_approval_status(&pending.approval_id, "rejected")
            .map_err(|e| RuntimeError::State(format!("failed to update rejection status: {e}")))?;

        self.must_emit_event(&thread_id, EventFrame::ToolCallRejected { turn_id: turn_id.clone(), call_id: pending.call_id.clone() }).await?;
        self.must_emit_event(&thread_id, EventFrame::TurnFailed { turn_id: turn_id.clone(), error: "tool call rejected by user".into() }).await?;

        let mut threads = self.inner.active_threads.write().await;
        if let Some(active) = threads.get_mut(&thread_id) { active.active_turn_id = None; }

        self.inner.state.update_turn_status(turn_id.as_str(), "failed", Some(current_timestamp()))
            .map_err(|e| RuntimeError::State(format!("{e}")))?;

        Ok(Turn {
            id: turn_id,
            thread_id,
            status: TurnStatus::Failed,
            user_input: String::new(),
            created_at: current_timestamp(),
            completed_at: Some(current_timestamp()),
            model: None,
            model_provider: None,
            parent_turn_id: None,
            parent_thread_id: None,
            subagent_id: None,
        })
    }

    /// Replay persisted events from durable state.
    pub fn replay_events(
        &self,
        thread_id: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, deepomni_protocol::EventFrame)>, RuntimeError> {
        self.inner.state.get_events_since(thread_id, since_seq)
            .map_err(|e| RuntimeError::State(format!("{e}")))
    }

    /// Shut down the runtime gracefully.
    pub async fn shutdown(&self) {
        // Close all active broadcast channels.
        let threads: Vec<ThreadId> = {
            self.inner.active_threads.read().await.keys().cloned().collect()
        };
        for tid in threads {
            self.inner.event_bus.close_thread(&tid).await;
        }
    }

    // ── Internal helpers ──

    async fn get_active_thread(&self, thread_id: &ThreadId) -> Result<ActiveThread, RuntimeError> {
        self.inner
            .active_threads
            .read()
            .await
            .get(thread_id)
            .cloned()
            .or_else(|| {
                // Try to load from state.
                let record = self.inner.state.get_thread(thread_id.as_str()).ok().flatten()?;
                Some(ActiveThread {
                    thread: Thread {
                        id: ThreadId::from_string(&record.id),
                        preview: record.preview,
                        ephemeral: record.ephemeral,
                        model_provider: record.model_provider,
                        created_at: record.created_at,
                        updated_at: record.updated_at,
                        status: serde_status(&record.status),
                        path: record.path,
                        cwd: std::path::PathBuf::from(&record.cwd),
                        cli_version: record.cli_version,
                        source: serde_source(&record.source),
                        name: record.name,
                        sandbox_policy: record.sandbox_policy,
                        approval_mode: record.approval_mode,
                        parent_thread_id: record.parent_thread_id.map(ThreadId::from_string),
                    },
                    status: serde_status(&record.status),
                    active_turn_id: None,
                })
            })
            .ok_or_else(|| RuntimeError::ThreadNotFound(thread_id.clone()))
    }

    fn persist_thread(&self, thread: &Thread) -> Result<(), RuntimeError> {
        use deepomni_state::ThreadRecord;
        let record = ThreadRecord {
            id: thread.id.to_string(),
            preview: thread.preview.clone(),
            ephemeral: thread.ephemeral,
            model_provider: thread.model_provider.clone(),
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            status: format!("{:?}", thread.status).to_lowercase(),
            path: thread.path.clone(),
            cwd: thread.cwd.display().to_string(),
            cli_version: thread.cli_version.clone(),
            source: format!("{:?}", thread.source).to_lowercase(),
            name: thread.name.clone(),
            sandbox_policy: thread.sandbox_policy.clone(),
            approval_mode: thread.approval_mode.clone(),
            archived: false,
            archived_at: None,
            parent_thread_id: thread.parent_thread_id.as_ref().map(|id| id.to_string()),
        };
        self.inner.state.upsert_thread(&record).map_err(|e| RuntimeError::State(format!("{e}")))
    }

    fn persist_turn(&self, turn: &Turn) -> Result<(), RuntimeError> {
        use deepomni_state::TurnRecord;
        let record = TurnRecord {
            id: turn.id.to_string(),
            thread_id: turn.thread_id.to_string(),
            status: format!("{:?}", turn.status).to_lowercase(),
            user_input: turn.user_input.clone(),
            created_at: turn.created_at,
            completed_at: turn.completed_at,
            model: turn.model.clone(),
            model_provider: turn.model_provider.clone(),
            parent_turn_id: turn.parent_turn_id.as_ref().map(|id| id.to_string()),
            parent_thread_id: turn.parent_thread_id.as_ref().map(|id| id.to_string()),
            subagent_id: turn.subagent_id.clone(),
        };
        self.inner.state.insert_turn(&record).map_err(|e| RuntimeError::State(format!("{e}")))
    }

    /// Reconstruct a ModelMessage transcript from persisted turn items.
    fn build_transcript_from_items(
        &self,
        turn_id: &TurnId,
    ) -> Vec<deepomni_model_provider::ModelMessage> {
        let items = self.inner.state.get_turn_items(turn_id.as_str()).unwrap_or_default();
        let mut messages: Vec<deepomni_model_provider::ModelMessage> = Vec::new();
        for (_seq, item) in items {
            match item {
                deepomni_protocol::TurnItem::UserMessage { content } => {
                    messages.push(deepomni_model_provider::ModelMessage {
                        role: deepomni_model_provider::MessageRole::User,
                        content: Some(content),
                        tool_calls: vec![],
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
                deepomni_protocol::TurnItem::AssistantDelta { .. }
                | deepomni_protocol::TurnItem::AssistantCompleted
                | deepomni_protocol::TurnItem::ReasoningBlock { .. } => {
                    // Deltas are aggregated; skip for transcript reconstruction.
                }
                deepomni_protocol::TurnItem::ToolCall { call_id, tool_name, arguments } => {
                    messages.push(deepomni_model_provider::ModelMessage {
                        role: deepomni_model_provider::MessageRole::Assistant,
                        content: None,
                        tool_calls: vec![deepomni_model_provider::ModelToolCall {
                            call_id: call_id.to_string(),
                            tool_name,
                            arguments,
                        }],
                        tool_call_id: None,
                        reasoning_content: None,
                    });
                }
                deepomni_protocol::TurnItem::ToolResult { call_id, tool_name, output_preview, success } => {
                    messages.push(deepomni_model_provider::ModelMessage {
                        role: deepomni_model_provider::MessageRole::Tool,
                        content: output_preview,
                        tool_calls: vec![],
                        tool_call_id: Some(call_id.to_string()),
                        reasoning_content: None,
                    });
                }
                deepomni_protocol::TurnItem::ApprovalDecision { .. } => {}
            }
        }
        messages
    }

    /// Emit an event (best-effort). For critical lifecycle events that MUST
    /// be durable, use `must_emit_event` which returns an error on failure.
    async fn emit_event(&self, thread_id: &ThreadId, event: EventFrame) {
        let seq = self.inner.state.next_event_seq(thread_id.as_str()).unwrap_or(0);
        if let Err(e) = self.inner.event_bus.emit(thread_id.clone(), seq, event).await {
            tracing::error!(%thread_id, seq, error = %e, "event persistence failed");
        }
    }

    /// Emit a critical lifecycle event. Returns error if persistence fails,
    /// so the caller can fail or roll back the state transition.
    async fn must_emit_event(&self, thread_id: &ThreadId, event: EventFrame) -> Result<(), RuntimeError> {
        let seq = self.inner.state.next_event_seq(thread_id.as_str()).unwrap_or(0);
        self.inner.event_bus.emit(thread_id.clone(), seq, event).await
            .map_err(|e| RuntimeError::State(format!("critical event persist failed: {e}")))
    }
}

// ── Helpers ──

fn tool_output_preview(output: &deepomni_protocol::tool::ToolOutput) -> String {
    match output {
        deepomni_protocol::tool::ToolOutput::Function { body, .. } => {
            body.as_ref()
                .map(|v| {
                    let s = serde_json::to_string(v).unwrap_or_default();
                    if s.len() > 200 { format!("{}...", &s[..200]) } else { s }
                })
                .unwrap_or_else(|| "ok".into())
        }
        deepomni_protocol::tool::ToolOutput::Mcp { result } => {
            let s = serde_json::to_string(result).unwrap_or_default();
            if s.len() > 200 { format!("{}...", &s[..200]) } else { s }
        }
    }
}

fn extract_turn_id(event: &EventFrame) -> &str {
    match event {
        EventFrame::TurnStarted { turn_id, .. }
        | EventFrame::TurnSteered { turn_id, .. }
        | EventFrame::TurnInterrupted { turn_id, .. }
        | EventFrame::TurnCompleted { turn_id, .. }
        | EventFrame::TurnFailed { turn_id, .. } => turn_id.as_str(),
        EventFrame::AssistantMessageDelta { turn_id, .. }
        | EventFrame::AssistantMessageCompleted { turn_id, .. }
        | EventFrame::AssistantReasoningDelta { turn_id, .. }
        | EventFrame::AssistantReasoningCompleted { turn_id, .. } => turn_id.as_str(),
        EventFrame::ToolCallStarted { turn_id, .. }
        | EventFrame::ToolCallArgumentsDelta { turn_id, .. }
        | EventFrame::ToolCallRequiresApproval { turn_id, .. }
        | EventFrame::ToolCallApproved { turn_id, .. }
        | EventFrame::ToolCallRejected { turn_id, .. }
        | EventFrame::ToolCallCompleted { turn_id, .. }
        | EventFrame::ToolCallFailed { turn_id, .. } => turn_id.as_str(),
        EventFrame::ContextCompactionStarted { turn_id, .. }
        | EventFrame::ContextCompactionCompleted { turn_id, .. } => turn_id.as_str(),
        _ => "",
    }
}

fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn serde_status(s: &str) -> ThreadStatus {
    match s {
        "running" => ThreadStatus::Running,
        "completed" => ThreadStatus::Completed,
        "failed" => ThreadStatus::Failed,
        "paused" => ThreadStatus::Paused,
        "archived" => ThreadStatus::Archived,
        _ => ThreadStatus::Idle,
    }
}

fn serde_source(s: &str) -> SessionSource {
    match s {
        "resume" => SessionSource::Resume,
        "fork" => SessionSource::Fork,
        "api" => SessionSource::Api,
        "unknown" => SessionSource::Unknown,
        _ => SessionSource::Interactive,
    }
}

// ── RuntimeError ──

#[derive(Debug)]
pub enum RuntimeError {
    Config(String),
    State(String),
    ThreadNotFound(ThreadId),
    InvalidTransition(String),
    NotReady(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Config(msg) => write!(f, "config error: {msg}"),
            RuntimeError::State(msg) => write!(f, "state error: {msg}"),
            RuntimeError::ThreadNotFound(id) => write!(f, "thread not found: {id}"),
            RuntimeError::InvalidTransition(msg) => write!(f, "invalid state transition: {msg}"),
            RuntimeError::NotReady(msg) => write!(f, "runtime not ready: {msg}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_construction() {
        let builder = RuntimeBuilder::new().workspace("/tmp/test");
        // Builder constructs without error. Build actually creates the runtime.
        let _ = builder;
    }

    #[test]
    fn test_serde_status_mapping() {
        assert_eq!(serde_status("running"), ThreadStatus::Running);
        assert_eq!(serde_status("idle"), ThreadStatus::Idle);
        assert_eq!(serde_status("unknown"), ThreadStatus::Idle);
    }

    #[test]
    fn test_serde_source_mapping() {
        assert_eq!(serde_source("interactive"), SessionSource::Interactive);
        assert_eq!(serde_source("resume"), SessionSource::Resume);
    }
}
