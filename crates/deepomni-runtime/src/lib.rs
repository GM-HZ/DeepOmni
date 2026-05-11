//! # DeepOmni Runtime
//!
//! Main host-agnostic runtime facade. Provides the `Runtime` struct and
//! `RuntimeBuilder` for creating, managing, and interacting with agent
//! threads, turns, approvals, and event subscriptions.
//!
//! PRD §7.3, §10.1

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{RwLock, Mutex};

use deepomni_agent::{TurnConfig, TurnResult, TurnRunner};
use deepomni_config::{ConfigStore, ResolvedConfig};
use deepomni_context::ContextManager;
use deepomni_events::EventBus;
use deepomni_journal::{journal_to_event_frame, JournalEntry, TurnJournal};
use deepomni_model_provider::{ModelProvider, ProviderRegistry};
use deepomni_policy::{AgentMode, PermissionProfile, PolicyEngine};
use deepomni_protocol::{
    CreateThreadRequest, CreateTurnRequest, EventFrame,
    Thread, ThreadStatus, Turn, TurnStatus, SessionSource,
};
use deepomni_protocol::id::{ThreadId, ToolCallId, TurnId};
use deepomni_state::StateStore;
use deepomni_tools::{ApprovalStore, ToolRegistry};

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
    #[allow(dead_code)]
    pub workspace: String,
    pub config: deepomni_agent::TurnConfig,
}

struct RuntimeInner {
    config: ResolvedConfig,
    state: Arc<StateStore>,
    services: Arc<SessionServices>,
    event_bus: Arc<EventBus>,
    tool_registry: Arc<ToolRegistry>,
    policy_engine: Arc<PolicyEngine>,
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    turn_runner: TurnRunner,
    skill_manager: Arc<RwLock<deepomni_skills::SkillManager>>,
    #[allow(dead_code)]
    plugin_manager: Arc<RwLock<deepomni_plugin::PluginManager>>,
    hook_dispatcher: Arc<deepomni_hooks::HookDispatcher>,
    active_threads: Arc<RwLock<HashMap<ThreadId, ActiveThread>>>,
    name_counter: Mutex<u64>,
    /// Pending approvals keyed by approval_id.
    pending_approvals: RwLock<HashMap<String, PendingApproval>>,
    /// Active sub-agents tracked by subagent_id.
    #[allow(dead_code)]
    subagent_registry: RwLock<HashMap<String, SubagentState>>,
    /// Threads with an active journal-to-event projection task.
    journal_projection_threads: RwLock<HashSet<ThreadId>>,
}

/// Long-lived service container shared by sessions and turn machinery. This is
/// intentionally additive while RuntimeInner is being decomposed.
pub struct SessionServices {
    pub state: Arc<StateStore>,
    pub journal: Arc<dyn TurnJournal>,
    pub event_bus: Arc<EventBus>,
    pub tool_registry: Arc<ToolRegistry>,
    pub policy_engine: Arc<PolicyEngine>,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    pub skill_manager: Arc<RwLock<deepomni_skills::SkillManager>>,
    pub plugin_manager: Arc<RwLock<deepomni_plugin::PluginManager>>,
    pub hook_dispatcher: Arc<deepomni_hooks::HookDispatcher>,
}

/// Runtime state for a spawned sub-agent.
#[allow(dead_code)]
struct SubagentState {
    pub subagent_id: String,
    pub parent_thread_id: ThreadId,
    pub parent_turn_id: TurnId,
    pub task: String,
    pub status: SubagentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
enum SubagentStatus {
    Running,
    Completed,
    Failed,
}

/// An active thread in the runtime.
#[derive(Debug, Clone)]
struct ActiveThread {
    pub thread: Thread,
    #[allow(dead_code)]
    pub status: ThreadStatus,
    pub active_turn_id: Option<TurnId>,
    #[allow(dead_code)]
    pub context_manager: ContextManager,
    pub approval_store: Arc<ApprovalStore>,
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

        // Initialize subsystems. Journal entries are the durable source of
        // truth; EventBus is now a live projection used by hosts/SSE.
        let event_bus = Arc::new(EventBus::new());
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
        let provider_registry = Arc::new(RwLock::new(provider_registry));
        let base_profile = PermissionProfile::new()
            .allow(deepomni_protocol::PermissionType::FilesystemRead)
            .allow(deepomni_protocol::PermissionType::FilesystemWrite);
        let policy_engine = Arc::new(PolicyEngine::new(AgentMode::Agent, base_profile));
        let turn_runner = TurnRunner::new(
            tool_registry.clone(),
            policy_engine.clone(),
            state.clone(),
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

        let skill_manager = Arc::new(RwLock::new(skill_manager));
        let plugin_manager = Arc::new(RwLock::new(plugin_manager));
        let hook_dispatcher = Arc::new(deepomni_hooks::HookDispatcher::new());
        let services = Arc::new(SessionServices {
            state: state.clone(),
            journal: state.clone(),
            event_bus: event_bus.clone(),
            tool_registry: tool_registry.clone(),
            policy_engine: policy_engine.clone(),
            provider_registry: provider_registry.clone(),
            skill_manager: skill_manager.clone(),
            plugin_manager: plugin_manager.clone(),
            hook_dispatcher: hook_dispatcher.clone(),
        });

        Ok(Runtime {
            inner: Arc::new(RuntimeInner {
                config,
                state,
                services,
                event_bus,
                tool_registry,
                policy_engine,
                provider_registry,
                turn_runner,
                skill_manager,
                plugin_manager,
                hook_dispatcher,
                active_threads: Arc::new(RwLock::new(HashMap::new())),
                name_counter: Mutex::new(0),
                pending_approvals: RwLock::new(HashMap::new()),
                subagent_registry: RwLock::new(HashMap::new()),
                journal_projection_threads: RwLock::new(HashSet::new()),
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

    /// Long-lived service container for the emerging engine/session split.
    pub fn session_services(&self) -> Arc<SessionServices> {
        self.inner.services.clone()
    }

    /// Internal turn journal handle used by engine components during the
    /// journal migration.
    pub fn turn_journal(&self) -> Arc<dyn TurnJournal> {
        self.inner.state.clone()
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
                context_manager: ContextManager::new(),
                approval_store: Arc::new(ApprovalStore::new()),
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
            if let Some(active) = guard.get_mut(tid)
                && active.active_turn_id.as_ref() == Some(tuid)
            {
                active.active_turn_id = None;
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

        self.append_turn_journal(
            &thread_id,
            &turn_id,
            JournalEntry::TurnStarted {
                user_input: request.input.clone(),
            },
        )?;

        // Resolve provider.
        let resolved_provider = {
            let registry = self.inner.provider_registry.read().await;
            registry.resolve(&model).map(|(_, p)| p.clone())
        };
        let provider_arc = match resolved_provider {
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
                approval_store: self.approval_store_for_thread(&thread_id).await,
                item_sink: None,
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

                self.append_turn_journal(
                    &thread_id,
                    &turn_id,
                    JournalEntry::TurnCompleted,
                )?;

                Ok(Turn {
                    status: TurnStatus::Completed,
                    completed_at: Some(completed_at),
                    ..turn
                })
            }
            Ok(TurnResult::NeedsApproval { call_id, tool_name, arguments, approval_id, .. }) => {
                // The durable pending approval row is projected from the
                // journal entry written by TurnRunner. Keep only the
                // session-local cache here for fast lookup.
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
                self.append_turn_journal(
                    &thread_id,
                    &turn_id,
                    JournalEntry::TurnFailed {
                        error: format!("{e}"),
                    },
                )?;

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
        self.ensure_journal_projection(thread_id.clone()).await;
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

        if let Some(store) = self.approval_store_for_thread(&pending.thread_id).await {
            store.put(
                serde_json::json!({
                    "tool_name": pending.tool_name.clone(),
                    "arguments": pending.arguments.clone(),
                }),
                deepomni_tools::ApprovalDecision::Approved,
            );
        }

        // Mark approved through the journal so pending_approvals remains a
        // materialized read model of journal state.
        self.append_turn_journal(
            &thread_id,
            &turn_id,
            JournalEntry::ApprovalResolved {
                approval_id: pending.approval_id.clone(),
                approved: true,
            },
        )?;

        let provider = {
            let registry = self.inner.provider_registry.read().await;
            registry.resolve(&pending.model)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| RuntimeError::NotReady(
                    format!("provider for model '{}' not available", pending.model)
                ))?
        };

        let transcript = self.build_transcript_from_items(&pending.turn_id);

        let result = self.inner.turn_runner.continue_after_approval(
            deepomni_agent::ResumeTurnRequest {
                thread_id: thread_id.clone(), turn_id: turn_id.clone(),
                call_id: pending.call_id.clone(), tool_name: pending.tool_name.clone(),
                arguments: pending.arguments.clone(), config: pending.config.clone(),
                provider, tool_result_content: String::new(), transcript,
                reasoning_to_replay: None, item_sink: None,
            },
        ).await;

        let final_status = match result {
            Ok(TurnResult::Completed { .. }) => {
                self.append_turn_journal(
                    &thread_id,
                    &turn_id,
                    JournalEntry::TurnCompleted,
                )?;
                TurnStatus::Completed
            }
            Ok(TurnResult::NeedsApproval { .. }) => TurnStatus::WaitingForApproval,
            Err(e) => {
                self.append_turn_journal(
                    &thread_id,
                    &turn_id,
                    JournalEntry::TurnFailed {
                        error: format!("{e}"),
                    },
                )?;
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

        self.append_turn_journal(
            &thread_id,
            &turn_id,
            JournalEntry::ApprovalResolved {
                approval_id: pending.approval_id.clone(),
                approved: false,
            },
        )?;

        self.append_turn_journal(
            &thread_id,
            &turn_id,
            JournalEntry::ToolCallRejected {
                call_id: pending.call_id.clone(),
            },
        )?;
        self.append_turn_journal(
            &thread_id,
            &turn_id,
            JournalEntry::TurnFailed {
                error: "tool call rejected by user".into(),
            },
        )?;

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
        let tid = ThreadId::from_string(thread_id);
        let journal_events: Vec<_> = self
            .inner
            .state
            .replay(&tid, since_seq)
            .map_err(|e| RuntimeError::State(format!("{e}")))?
            .into_iter()
            .filter_map(|record| {
                journal_to_event_frame(&record).map(|event| (record.seq, event))
            })
            .collect();
        if !journal_events.is_empty() {
            return Ok(journal_events);
        }

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

    #[allow(dead_code)]
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
                    context_manager: ContextManager::new(),
                    approval_store: Arc::new(ApprovalStore::new()),
                })
            })
            .ok_or_else(|| RuntimeError::ThreadNotFound(thread_id.clone()))
    }

    async fn approval_store_for_thread(&self, thread_id: &ThreadId) -> Option<Arc<ApprovalStore>> {
        self.inner
            .active_threads
            .read()
            .await
            .get(thread_id)
            .map(|active| active.approval_store.clone())
    }

    async fn ensure_journal_projection(&self, thread_id: ThreadId) {
        {
            let mut projected = self.inner.journal_projection_threads.write().await;
            if !projected.insert(thread_id.clone()) {
                return;
            }
        }

        let mut journal_subscriber = self.inner.state.subscribe(thread_id);
        let event_bus = self.inner.event_bus.clone();
        tokio::spawn(async move {
            loop {
                match journal_subscriber.recv().await {
                    Ok(record) => {
                        if let Some(event) = journal_to_event_frame(&record) {
                            let _ = event_bus
                                .emit(record.thread_id.clone(), record.seq, event)
                                .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn append_turn_journal(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        entry: JournalEntry,
    ) -> Result<(), RuntimeError> {
        self.inner
            .state
            .append(thread_id, turn_id, entry)
            .map(|_| ())
            .map_err(|e| RuntimeError::State(format!("journal append failed: {e}")))
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
        let journal_messages = self.build_transcript_from_journal(turn_id);
        if !journal_messages.is_empty() {
            return journal_messages;
        }

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
                deepomni_protocol::TurnItem::ToolResult { call_id, tool_name: _, output_preview, success: _ } => {
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

    fn build_transcript_from_journal(
        &self,
        turn_id: &TurnId,
    ) -> Vec<deepomni_model_provider::ModelMessage> {
        let records = self
            .inner
            .state
            .get_journal_records_for_turn(turn_id.as_str())
            .unwrap_or_default();
        let mut messages = Vec::new();
        for record in records {
            match record.entry {
                JournalEntry::ApprovalPending {
                    call_id,
                    tool_name,
                    arguments,
                    ..
                }
                | JournalEntry::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                } if !arguments.is_null() => {
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
                JournalEntry::ToolCallCompleted {
                    call_id, output, ..
                } => {
                    messages.push(deepomni_model_provider::ModelMessage {
                        role: deepomni_model_provider::MessageRole::Tool,
                        content: output,
                        tool_calls: vec![],
                        tool_call_id: Some(call_id.to_string()),
                        reasoning_content: None,
                    });
                }
                _ => {}
            }
        }
        messages
    }

    /// Emit a legacy public event. Turn-scoped events now prefer the journal;
    /// this path remains for thread-level compatibility during migration.
    async fn emit_event(&self, thread_id: &ThreadId, event: EventFrame) {
        let seq = self.inner.state.next_event_seq(thread_id.as_str()).unwrap_or(0);
        if let Err(e) = self.inner.event_bus.emit(thread_id.clone(), seq, event).await {
            tracing::error!(%thread_id, seq, error = %e, "event persistence failed");
        }
    }

}

// ── Helpers ──

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
    use deepomni_journal::JournalEntry;

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

    #[tokio::test]
    async fn journal_append_is_projected_to_event_subscribers() {
        let state_path = std::env::temp_dir()
            .join(format!("runtime-journal-projection-{}.db", uuid::Uuid::new_v4()));
        let runtime = RuntimeBuilder::new()
            .workspace("/tmp/test")
            .state_path(state_path)
            .build()
            .await
            .unwrap();
        let thread = runtime
            .create_thread(CreateThreadRequest {
                workspace: "/tmp/test".into(),
                model: None,
                name: None,
                model_provider: None,
                approval_policy: None,
                sandbox: None,
                parent_thread_id: None,
                ephemeral: false,
            })
            .await
            .unwrap();
        let turn_id = TurnId::from_string("turn-projection-1");
        let mut subscriber = runtime.subscribe(thread.id.clone()).await;

        runtime
            .turn_journal()
            .append(
                &thread.id,
                &turn_id,
                JournalEntry::TurnStarted {
                    user_input: "hello from journal".into(),
                },
            )
            .unwrap();

        let envelope = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            subscriber.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        match envelope.frame {
            EventFrame::TurnStarted {
                thread_id,
                turn_id: projected_turn_id,
                user_input,
            } => {
                assert_eq!(thread_id, thread.id);
                assert_eq!(projected_turn_id, turn_id);
                assert_eq!(user_input, "hello from journal");
            }
            other => panic!("unexpected projected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_events_projects_journal_records_before_legacy_events() {
        let state_path = std::env::temp_dir()
            .join(format!("runtime-journal-replay-{}.db", uuid::Uuid::new_v4()));
        let runtime = RuntimeBuilder::new()
            .workspace("/tmp/test")
            .state_path(state_path)
            .build()
            .await
            .unwrap();
        let thread = runtime
            .create_thread(CreateThreadRequest {
                workspace: "/tmp/test".into(),
                model: None,
                name: None,
                model_provider: None,
                approval_policy: None,
                sandbox: None,
                parent_thread_id: None,
                ephemeral: false,
            })
            .await
            .unwrap();
        let turn_id = TurnId::from_string("turn-replay-journal-1");

        runtime
            .turn_journal()
            .append(
                &thread.id,
                &turn_id,
                JournalEntry::TurnStarted {
                    user_input: "journal replay".into(),
                },
            )
            .unwrap();

        let events = runtime.replay_events(thread.id.as_str(), 0).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, 1);
        match &events[0].1 {
            EventFrame::TurnStarted { user_input, .. } => {
                assert_eq!(user_input, "journal replay");
            }
            other => panic!("unexpected replay event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn submit_turn_replay_includes_journal_lifecycle_events() {
        let state_path = std::env::temp_dir()
            .join(format!("runtime-submit-journal-{}.db", uuid::Uuid::new_v4()));
        let runtime = RuntimeBuilder::new()
            .workspace("/tmp/test")
            .state_path(state_path)
            .build()
            .await
            .unwrap();
        runtime
            .register_provider(Arc::new(deepomni_test_support::MockModelProvider::new(vec![
                deepomni_test_support::mock_text_response("hello from model"),
            ])))
            .await;
        let thread = runtime
            .create_thread(CreateThreadRequest {
                workspace: "/tmp/test".into(),
                model: None,
                name: None,
                model_provider: None,
                approval_policy: None,
                sandbox: None,
                parent_thread_id: None,
                ephemeral: false,
            })
            .await
            .unwrap();

        runtime
            .submit_turn(
                thread.id.clone(),
                CreateTurnRequest {
                    input: "say hello".into(),
                    model: Some("mock-model".into()),
                    parent_turn_id: None,
                    subagent_id: None,
                    max_token_budget: Some(100),
                },
            )
            .await
            .unwrap();

        let events = runtime.replay_events(thread.id.as_str(), 0).unwrap();
        assert!(events.iter().any(|(_, event)| matches!(
            event,
            EventFrame::TurnStarted { user_input, .. } if user_input == "say hello"
        )));
        assert!(events.iter().any(|(_, event)| matches!(
            event,
            EventFrame::AssistantMessageDelta { delta, .. } if delta == "hello from model"
        )));
        assert!(events.iter().any(|(_, event)| matches!(
            event,
            EventFrame::TurnCompleted { .. }
        )));
    }

    #[tokio::test]
    async fn submit_turn_needing_approval_uses_journal_projection_once() {
        let state_path = std::env::temp_dir()
            .join(format!("runtime-approval-journal-{}.db", uuid::Uuid::new_v4()));
        let runtime = RuntimeBuilder::new()
            .workspace("/tmp/test")
            .state_path(state_path)
            .build()
            .await
            .unwrap();
        runtime
            .register_provider(Arc::new(deepomni_test_support::MockModelProvider::new(vec![
                deepomni_test_support::mock_tool_call_response(
                    "write_file",
                    serde_json::json!({"path": "approval.txt", "content": "hello"}),
                ),
            ])))
            .await;
        let thread = runtime
            .create_thread(CreateThreadRequest {
                workspace: "/tmp/test".into(),
                model: None,
                name: None,
                model_provider: None,
                approval_policy: None,
                sandbox: None,
                parent_thread_id: None,
                ephemeral: false,
            })
            .await
            .unwrap();

        let turn = runtime
            .submit_turn(
                thread.id.clone(),
                CreateTurnRequest {
                    input: "write approval file".into(),
                    model: Some("mock-model".into()),
                    parent_turn_id: None,
                    subagent_id: None,
                    max_token_budget: Some(100),
                },
            )
            .await
            .unwrap();

        assert_eq!(turn.status, TurnStatus::WaitingForApproval);
        let events = runtime.replay_events(thread.id.as_str(), 0).unwrap();
        assert!(events.iter().any(|(_, event)| matches!(
            event,
            EventFrame::ToolCallRequiresApproval { tool_name, .. } if tool_name == "write_file"
        )));
    }

    #[tokio::test]
    async fn transcript_reconstruction_prefers_journal_entries() {
        let state_path = std::env::temp_dir()
            .join(format!("runtime-journal-transcript-{}.db", uuid::Uuid::new_v4()));
        let runtime = RuntimeBuilder::new()
            .workspace("/tmp/test")
            .state_path(state_path)
            .build()
            .await
            .unwrap();
        let thread = runtime
            .create_thread(CreateThreadRequest {
                workspace: "/tmp/test".into(),
                model: None,
                name: None,
                model_provider: None,
                approval_policy: None,
                sandbox: None,
                parent_thread_id: None,
                ephemeral: false,
            })
            .await
            .unwrap();
        let turn_id = TurnId::from_string("turn-journal-transcript-1");
        let call_id = ToolCallId::from_string("call-journal-1");
        runtime
            .turn_journal()
            .append(
                &thread.id,
                &turn_id,
                JournalEntry::ApprovalPending {
                    call_id: call_id.clone(),
                    approval_id: "approval-journal-1".into(),
                    tool_name: "write_file".into(),
                    arguments: serde_json::json!({"path": "journal.txt"}),
                    reason: "mutating tool".into(),
                    model: "mock-model".into(),
                    workspace: "/tmp/test".into(),
                    config_json: "{}".into(),
                },
            )
            .unwrap();

        let transcript = runtime.build_transcript_from_items(&turn_id);

        assert_eq!(transcript.len(), 1);
        assert_eq!(
            transcript[0].role,
            deepomni_model_provider::MessageRole::Assistant
        );
        assert_eq!(transcript[0].tool_calls.len(), 1);
        assert_eq!(transcript[0].tool_calls[0].call_id, call_id.to_string());
        assert_eq!(transcript[0].tool_calls[0].tool_name, "write_file");
        assert_eq!(
            transcript[0].tool_calls[0].arguments,
            serde_json::json!({"path": "journal.txt"})
        );
    }
}
