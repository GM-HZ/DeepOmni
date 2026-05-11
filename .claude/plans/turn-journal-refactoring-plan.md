# DeepOmni Architecture Upgrade Plan

## Context

After 22 iterations of development, DeepOmni has 25 crates, 167 tests, and a working agent loop. The goal is to **converge the architecture toward the proven patterns from `ref/codex` (Rust, ~100 crates) and `ref/claude-code` (TypeScript)**, absorbing their core strengths while keeping DeepOmni's own identity.

### Current Architecture Gaps (vs reference projects)

**Data pipeline issues** (original scope):
1. **Three separate stores for turn events**: `events` table, `turn_items` table, and `pending_approvals` table write independently with no transaction boundary.
2. **TurnRunner directly depends on EventBus**: `Arc<EventBus>` field with placeholder `seq=0`.
3. **Request builder reuses new-turn logic for continuations**: `history.is_empty()` branching.

**Structural gaps** (identified from ref project comparison):
4. **Anemic TurnContext**: DeepOmni's `TurnExecutionContext` has 6 fields; Codex's `TurnContext` has 40+ fields as a frozen per-turn snapshot (provider, model_info, approval_policy, permission_profile, environments, tools_config, features, timing, etc).
5. **No ContextManager**: History is raw `Vec<ModelMessage>` passed around. Codex has a token-aware `ContextManager` with `record_items(truncation_policy)`, `for_prompt(input_modalities)`, `replace_history()`, and version tracking for compaction/rollback.
6. **God-object RuntimeInner**: 1039-line `Runtime` mixes session config, state, event bus, tool registry, policy engine, provider registry, skill/plugin/hook managers, active_threads, pending_approvals. Codex separates these into `Session` (long-lived) + `SessionServices` (service container, 20+ services) + `SessionState` (mutable state + ContextManager).
7. **No parallel tool execution**: Strictly serial. Codex uses `RwLock` (read=parallel, write=exclusive) in `ToolCallRuntime`; Claude Code uses `StreamingToolExecutor` with concurrent-safe/exclusive classification.
8. **No approval session cache**: Every tool call requires re-approval. Codex's `ApprovalStore` caches `ReviewDecision` per serialized key for session-lifetime skip.
9. **No StateStore trait abstraction**: Concrete SQLite implementation only. Codex has `ThreadStore` async trait with `LocalThreadStore` + `InMemoryThreadStore` for testing.
10. **No multi-agent communication**: `spawn_subagent` is just recursive `run_turn`. Codex has `Mailbox`(mpsc) + `AgentControl` + `AgentRegistry` + `MailboxDeliveryPhase` state machine.
11. **No ContextualUserFragment protocol**: Context injection is untracked. Codex's `ContextualUserFragment` trait provides `START_MARKER`/`END_MARKER` for trackable, rollback-safe context injection.

## Goal

Two-phase architecture upgrade:

**Phase A (Stages 1–6)**: Unify the data pipeline — converge `events`/`turn_items`/`pending_approvals` into a single journal with EventFrame as projection.

**Phase B (Stages 7–12)**: Absorb reference project architecture — bring Codex's ContextManager, Session/TurnContext separation, parallel tool execution, approval caching, and Claude Code's streaming tool executor patterns into DeepOmni.

```
Phase A (data pipeline):                Phase B (architecture):
┌──────────┐    ┌──────────────┐        ┌─────────────────────────────────────────┐
│ EventBus │    │  TurnJournal │        │  Session (long-lived)                   │
├──────────┤ →  │              │        │  ├─ SessionServices (service container) │
│TurnItems │    │              │        │  ├─ SessionState + ContextManager       │
├──────────┤    └──────────────┘        │  └─ TurnContext (40+ field snapshot)    │
│PendingAp │                            │     ├─ ApprovalStore (session cache)    │
└──────────┘                            │     └─ ToolCallRuntime (parallel exec)  │
                                        └─────────────────────────────────────────┘
```

## Design Decisions (from review)

1. **JournalEntry ≠ EventFrame**: JournalEntry is the internal complete record. EventFrame stays as the public, stable, host-safe projection. TurnItem is deprecated.
2. **pending_approvals table stays**: As a materialized read model, updated synchronously from journal writes within the same transaction.
3. **TurnJournal trait lives in new `deepomni-journal` crate** at L1, not in protocol. StateStore implements it.
4. **New crate naming**: `deepomni-engine` for Session/Turn FSM, not `deepomni-core-runtime` (avoids confusion with existing `deepomni-core`).
5. **RequestCompiler gets three explicit paths**, not one function with `history.is_empty()` branching.
6. **EventFrame remains**: SSE/SDK clients get the stable projection. Journal is the engine's internal contract.
7. **JournalEntry stays out of `deepomni-protocol`**: `deepomni-protocol` remains the public/wire contract. `deepomni-journal` owns `JournalEntry` and may depend on protocol IDs/types; protocol must not depend on or re-export journal types.
8. **JournalRecord is the replay/subscription unit**: `JournalEntry` is the payload. `JournalRecord` wraps it with `seq`, `thread_id`, `turn_id`, and `created_at` so projection, replay, and subscribers never need hidden context.
9. **Projection bridge comes before writer migration**: Build journal persistence and EventFrame projection first, then switch `TurnRunner` from EventBus to TurnJournal. This keeps SSE/live events working during migration.
10. **Internal journal may contain more data than EventFrame**: Tool results and approval snapshots must store enough internal data for continuation/resume; EventFrame remains redacted/host-safe.

## Target Crate Hierarchy

```
L0 foundation:
  deepomni-core          error/result/redact
  deepomni-protocol      wire types, EventFrame, IDs
  deepomni-config        config types

L1 primitives:
  deepomni-state         StateStore (implements TurnJournal trait)
  deepomni-journal       TurnJournal trait, JournalEntry, JournalRecord,
                         JournalSubscriber, projection helpers (NEW)
  deepomni-events        EventBus (public EventFrame broadcast only)
  deepomni-policy        AgentMode, ApprovalPolicy
  deepomni-sandbox       SandboxManager
  deepomni-model-provider  ModelProvider trait, registry
  deepomni-tools         ToolHandler, ToolOrchestrator

L2 capability/context:
  deepomni-capabilities  CapabilityRegistry
  deepomni-context       ContextFragment, TokenBudget
  deepomni-prompt        PromptBuilder, cache hash
  deepomni-provider-deepseek
  deepomni-tool-builtin
  deepomni-mcp / skills / plugin / hooks

L3 engine:
  deepomni-engine        SessionManager, TurnStateMachine, RequestCompiler,
                         ToolTransactionEngine, ApprovalCoordinator (NEW)
  deepomni-agent         TurnRunner (depends on TurnJournal, not EventBus)

L4 hosts:
  deepomni-runtime       SDK-facing facade
  deepomni-server        HTTP/SSE
  deepomni-cli           debug CLI
```

## Implementation Stages

## Implementation Progress

- 2026-05-10: Stage 1 foundation implemented: added `deepomni-journal` crate, `JournalEntry`, `JournalRecord`, `TurnJournal`, `JournalSubscriber`, and initial projection helper tests.
- 2026-05-10: Stage 2 foundation implemented: `StateStore` now implements `TurnJournal`, persists `journal_entries`, replays `JournalRecord`s, broadcasts journal subscribers, and synchronously projects approval pending/resolved entries into `pending_approvals`.
- 2026-05-10: Stage 3 started: added `journal_to_event_frame()` helper in `deepomni-journal`; runtime/EventBus wiring remains next.
- 2026-05-11: Stage 3 runtime bridge implemented: `Runtime::subscribe()` starts one journal-to-EventFrame projection task per thread; `Runtime::turn_journal()` exposes the journal handle for engine components during migration.
- 2026-05-11: Stage 4 writer migration started: `TurnRunner` now depends on `Arc<dyn TurnJournal>` instead of `EventBus`; agent tests use `InMemoryJournal`. Legacy `item_sink` and some runtime-level event persistence paths remain for the Stage 5 cleanup.
- 2026-05-11: Stage 5 started: `Runtime::replay_events()` now prefers projected journal records and falls back to legacy `events` only when no journal records exist.
- 2026-05-11: Stage 5 continued: runtime turn lifecycle (`TurnStarted`, `TurnCompleted`, `TurnFailed`, rejection events) now writes journal entries directly. Durable pending approval writes are no longer duplicated in runtime; `pending_approvals` is populated by the journal projection inside `StateStore`.
- 2026-05-11: Stage 5 continued: approval resume transcript reconstruction is now journal-first with `turn_items` as fallback, and runtime no longer writes new `turn_items` through `item_sink`.
- 2026-05-11: Stage 6 started: added `RequestCompiler` with explicit `compile_new_turn()` and `compile_tool_continuation()` paths; `TurnRunner` now uses those paths instead of embedding `history.is_empty()` inside request message assembly.
- 2026-05-11: Stage 6 completed: `deepomni-engine::EngineRequestCompiler` adds the explicit approval-resume path over `TurnJournal` replay.
- 2026-05-11: Stage 7 implemented: `deepomni-context` now owns `ContextManager`, `TruncationPolicy`, `ContextSnapshot`, and `TokenUsageInfo` with ordering, truncation, compaction-threshold, and replacement tests.
- 2026-05-11: Stage 8 implemented as the migration target: `deepomni-agent::TurnContext` now captures provider, policy, permission profile, cwd/env, token budget, `ContextManager`, journal, and reasoning replay.
- 2026-05-11: Stage 9 implemented as an engine crate scaffold: added `deepomni-engine` with `SessionManager`, `SessionState`, `TurnStateMachine`, `EngineRequestCompiler`, `ToolTransactionEngine`, and `ApprovalCoordinator`.
- 2026-05-11: Stage 10 implemented additively: `Runtime` now exposes a `SessionServices` service container and active threads hold `ContextManager` plus session-scoped `ApprovalStore`.
- 2026-05-11: Stage 11 implemented: added `deepomni-tools::ApprovalStore`, cache-aware orchestrator evaluation, runtime approve-time cache population, and tests for cache hits/rejections.
- 2026-05-11: Stage 12 implemented: added `deepomni-tools::ToolCallRuntime` and `deepomni-engine::ToolTransactionEngine`; non-mutating parallel-safe calls run under shared read access with a concurrency test.
- 2026-05-11: Final verification passed: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.

### Stage 1: `deepomni-journal` crate + JournalEntry

**Duration**: ~30 min

Create new crate `deepomni-journal` at L1:

```rust
// crates/deepomni-journal/src/lib.rs

/// The single source of truth for everything that happens in a turn.
/// Replaces the separate EventFrame + TurnItem + PendingApprovalRecord paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEntry {
    // Turn lifecycle
    TurnStarted { user_input: String },
    TurnCompleted,
    TurnFailed { error: String },
    
    // Assistant output
    AssistantDelta { message_id: MessageId, delta: String },
    AssistantCompleted { message_id: MessageId },
    ReasoningBlock { message_id: MessageId, content: String, replay_required: bool },
    
    // Tool lifecycle
    ToolCallRequested { call_id: ToolCallId, tool_name: String, arguments: Value },
    ToolCallArgumentsDelta { call_id: ToolCallId, delta: String },
    ToolCallApproved { call_id: ToolCallId },
    ToolCallRejected { call_id: ToolCallId },
    ToolCallCompleted {
        call_id: ToolCallId,
        success: bool,
        /// Full internal content used for model continuation.
        output: Option<String>,
        /// Host-safe projection used by EventFrame.
        output_preview: Option<String>,
    },
    ToolCallFailed { call_id: ToolCallId, error: String },
    
    // Approval
    ApprovalPending {
        call_id: ToolCallId,
        approval_id: String,
        tool_name: String,
        arguments: Value,
        reason: String,
        /// Resume snapshot for the materialized pending_approvals read model.
        model: String,
        workspace: String,
        config_json: String,
    },
    ApprovalResolved { approval_id: String, approved: bool },

    // Sub-agents
    SubagentSpawned { parent_turn_id: TurnId, subagent_id: SubagentId, task: String },
    SubagentCompleted { parent_turn_id: TurnId, subagent_id: SubagentId, result_summary: String },
    SubagentFailed { parent_turn_id: TurnId, subagent_id: SubagentId, error: String },
    
    // Metadata
    ContextCompaction { reason: String },
    ProviderUsage { prompt_tokens: u64, completion_tokens: u64 },
}

/// Durable/replayable journal envelope. This is the smallest unit emitted
/// to subscribers and returned by replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalRecord {
    pub seq: i64,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub entry: JournalEntry,
    pub created_at: i64,
}

/// Trait for append-only turn journal. StateStore implements this.
pub trait TurnJournal: Send + Sync {
    /// Append a journal entry. Returns the durable journal record with the
    /// assigned monotonic sequence number.
    fn append(&self, thread_id: &ThreadId, turn_id: &TurnId, entry: JournalEntry) -> Result<JournalRecord>;
    
    /// Replay entries since a given sequence (exclusive).
    fn replay(&self, thread_id: &ThreadId, since_seq: i64) -> Result<Vec<JournalRecord>>;
    
    /// Subscribe to live journal entries.
    fn subscribe(&self, thread_id: ThreadId) -> JournalSubscriber;
}

pub struct JournalSubscriber { /* wraps broadcast::Receiver<JournalRecord> */ }
```

**Files to create**:
- `crates/deepomni-journal/Cargo.toml`
- `crates/deepomni-journal/src/lib.rs`

**Files to modify**:
- `Cargo.toml` (workspace members): add `deepomni-journal`

**Verification**: `cargo build -p deepomni-journal` compiles, `cargo test -p deepomni-journal` passes.

### Stage 2: StateStore implements TurnJournal

**Duration**: ~45 min

Add `journal_entries` table to StateStore schema:

```sql
CREATE TABLE IF NOT EXISTS journal_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    entry_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(thread_id, seq)
);
```

Implement `TurnJournal` trait on `StateStore`:
- `append()`: BEGIN IMMEDIATE, compute max seq+1, insert, COMMIT. If entry is `ApprovalPending`, also insert into `pending_approvals` read model. If entry is `ApprovalResolved`, also update `pending_approvals` status. ALL in one transaction.
- `replay()`: SELECT from journal_entries WHERE thread_id = ? AND seq > ? ORDER BY seq.
- `subscribe()`: returns journal records from a journal-owned broadcast channel. It must not delegate to EventBus; EventBus is only the public EventFrame projection layer.

**Files to modify**:
- `crates/deepomni-state/Cargo.toml`: add `deepomni-journal` dep
- `crates/deepomni-state/src/lib.rs`: add `journal_entries` table, impl `TurnJournal`
- `crates/deepomni-journal/src/lib.rs`: define `JournalProjection` helper

**Verification**: `cargo test -p deepomni-state` — new test: append JournalEntry, replay, verify seq monotonicity.

### Stage 3: Journal projection bridge for EventFrame/SSE

**Duration**: ~60 min

Build the compatibility bridge before changing writers:

1. Add `journal_to_event_frame(record: &JournalRecord) -> Option<EventFrame>` in `deepomni-journal` or a small projection module.
2. Runtime wires a projection task/callback that observes journal records and broadcasts projected `EventFrame`s through `EventBus`.
3. `EventBus` stops owning persistence responsibility for journal-sourced events. During the transition, legacy `must_emit_event` can still write public events to the old `events` table until all writers are migrated.
4. SSE replay gains a journal-backed path by projecting `journal.replay()` records. Keep old `events` replay as a fallback only while legacy writers still exist.

**Important**: This stage exists before migrating `TurnRunner` so live event subscribers keep working while the writer path moves from EventBus to TurnJournal.

**Files to modify**:
- `crates/deepomni-journal/src/lib.rs`: add EventFrame projection helper
- `crates/deepomni-events/src/lib.rs`: make EventBus public-broadcast-only for journal-sourced events
- `crates/deepomni-runtime/src/lib.rs`: wire journal projection to EventBus
- `crates/deepomni-server/src/lib.rs`: prefer journal-backed replay when available

**Verification**: `cargo test --workspace`, SSE replay test with journal records.

### Stage 4: TurnRunner depends on TurnJournal, not EventBus

**Duration**: ~60 min

Change `TurnRunner` struct:

```rust
// Before
pub struct TurnRunner {
    tool_registry: Arc<ToolRegistry>,
    policy_engine: Arc<PolicyEngine>,
    event_bus: Arc<EventBus>,           // ← REMOVE
}

// After
pub struct TurnRunner {
    tool_registry: Arc<ToolRegistry>,
    policy_engine: Arc<PolicyEngine>,
    journal: Arc<dyn TurnJournal>,       // ← ADD
}
```

The private `emit()` helper changes from `event_bus.emit(..., event)` to `journal.append(..., JournalEntry::...)`. All 18+ call sites in `run_turn` and `continue_after_approval` convert from `EventFrame` variants to `JournalEntry` variants.

The `item_sink` callback on `TurnExecutionContext` is deprecated — `ctx.record(TurnItem::...)` calls are replaced with `journal.append(JournalEntry::...)`.

**Files to modify**:
- `crates/deepomni-agent/Cargo.toml`: replace `deepomni-events` with `deepomni-journal`
- `crates/deepomni-agent/src/lib.rs`: TurnRunner field change, emit → journal.append
- `crates/deepomni-agent/tests/full_turn.rs`: update test setup
- `crates/deepomni-agent/tests/subagent.rs`: update test setup
- `crates/deepomni-runtime/Cargo.toml`: add `deepomni-journal`
- `crates/deepomni-runtime/src/lib.rs`: RuntimeBuilder::build to inject journal into TurnRunner

**Verification**: `cargo test --workspace` passes (all 167 tests must still pass).

### Stage 5: Retire EventBus persistence and legacy event replay

**Duration**: ~60 min

`EventBus` no longer calls `StateStore::append_event` directly. Instead:

1. All turn writers append journal records.
2. SSE replay reads from `journal.replay()` instead of `state.get_events_since()`.
3. The `persist_fn` callback on EventBus is removed — journal entries are the source of truth.
4. Legacy `events` and `turn_items` tables remain for compatibility/migration only; new turn data is read from journal.

```rust
// Journal → EventFrame projection
fn journal_to_event_frame(entry: &JournalEntry, thread_id: &ThreadId, turn_id: &TurnId) -> Option<EventFrame> {
    match entry {
        JournalEntry::TurnStarted { user_input } => Some(EventFrame::TurnStarted { ... }),
        JournalEntry::AssistantDelta { delta, .. } => Some(EventFrame::AssistantMessageDelta { ... }),
        // ... etc
    }
}
```

**Files to modify**:
- `crates/deepomni-events/src/lib.rs`: remove `persist_fn`, add `from_journal()` constructor
- `crates/deepomni-events/Cargo.toml`: add `deepomni-journal`
- `crates/deepomni-runtime/src/lib.rs`: remove EventBus::with_persistence wiring, event emits go through journal
- `crates/deepomni-server/src/lib.rs`: SSE replay uses journal.replay()

**Verification**: `cargo test --workspace`, SSE endpoint integration test.

### Stage 6: RequestCompiler — three explicit paths

**Duration**: ~60 min

Extract from `build_request_messages_from_assembled` into a dedicated struct:

```rust
pub struct RequestCompiler {
    provider: ProviderKind,
}

impl RequestCompiler {
    /// New turn: [system] + [context_fragments] + [history] + [user_input]
    pub fn compile_new_turn(&self, config: &TurnConfig, fragments: &[ContextFragment], user_input: &str) -> ModelRequest;
    
    /// Tool continuation: [system] + [context] + [history] + [assistant(tool_calls)] + [tool_results]
    pub fn compile_tool_continuation(&self, config: &TurnConfig, fragments: &[ContextFragment], transcript: &[ModelMessage]) -> ModelRequest;
    
    /// Approval resume: reconstruct from journal, build continuation
    pub fn compile_approval_resume(&self, config: &TurnConfig, fragments: &[ContextFragment], journal: &dyn TurnJournal, turn_id: &str) -> ModelRequest;
}
```

This eliminates the `history.is_empty()` branching. The three paths have guaranteed message ordering.

**Files to modify**:
- `crates/deepomni-agent/src/lib.rs`: replace `build_request_messages_from_assembled` with `RequestCompiler`

**Verification**: `cargo test --workspace`, specifically the full_turn integration tests.

### Stage 7: ContextManager — Token-Aware History (from Codex `ContextManager`)

**Priority**: High — required before fat TurnContext to settle transcript ownership.

**Problem**: History is raw `Vec<ModelMessage>` passed around with no token tracking, no truncation policy, no compaction support. Codex has a `ContextManager` with `record_items(truncation_policy)`, `for_prompt(input_modalities)`, `replace_history()`, token info tracking, and version-based compaction/rollback.

**Design**: Add `ContextManager` to `deepomni-context`:

```rust
// crates/deepomni-context/src/context_manager.rs
pub struct ContextManager {
    /// Append-only transcript (oldest first).
    items: Vec<ModelMessage>,
    /// Bumped on compaction/rollback.
    history_version: u64,
    /// Token usage from last API response.
    token_info: Option<TokenUsageInfo>,
    /// Baseline for settings diffing between turns.
    reference_context: Option<ContextSnapshot>,
}

impl ContextManager {
    pub fn new() -> Self;
    /// Append items with truncation policy applied.
    pub fn record_items(&mut self, items: &[ModelMessage], policy: TruncationPolicy);
    /// Prepare history for model call (normalize, filter, strip).
    pub fn for_prompt(&self) -> Vec<ModelMessage>;
    /// Replace history after compaction.
    pub fn replace_history(&mut self, items: Vec<ModelMessage>);
    /// Update token tracking from API response.
    pub fn update_token_info(&mut self, usage: &Usage, context_window: Option<u64>);
    /// Estimate if auto-compaction is needed.
    pub fn needs_compaction(&self, threshold: f64) -> bool;
}
```

**Key change**: `SessionState` (or the planned engine) holds a `ContextManager` per thread/session, replacing the raw `conversation_history: Vec<ModelMessage>` passing pattern. `RequestCompiler` delegates to `context_manager.for_prompt()`.

**Files to modify**:
- `crates/deepomni-context/src/lib.rs`: Add `context_manager` module
- `crates/deepomni-agent/src/lib.rs`: Use `ContextManager` instead of raw `Vec<ModelMessage>`
- `crates/deepomni-runtime/src/lib.rs`: Hold `ContextManager` per thread

**Verification**: `cargo test --workspace`, new test: record items → for_prompt → verify ordering and truncation.

### Stage 8: TurnContext Fat Snapshot (from Codex `TurnContext`)

**Priority**: Extreme — this is the single most impactful change.

**Problem**: DeepOmni's `TurnExecutionContext` has 6 fields. Codex's `TurnContext` has 40+ fields as a frozen per-turn snapshot. The agent currently reaches back into Runtime for provider, approval policy, permission profile, etc.

**Design**: Expand `TurnExecutionContext` into a Codex-style fat snapshot:

```rust
// crates/deepomni-agent/src/lib.rs
pub struct TurnContext {
    // Identity (existing)
    pub config: TurnConfig,
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    pub user_input: String,
    pub tool_specs: Vec<ToolSpec>,

    // Provider and transcript ownership.
    pub provider: Arc<dyn ModelProvider>,
    pub context_manager: ContextManager,

    // Policy (currently only AgentMode in TurnConfig)
    pub approval_policy: ApprovalPolicy,
    pub permission_profile: PermissionProfile,

    // Environment (from Codex TurnContext)
    pub cwd: PathBuf,
    pub environment_vars: HashMap<String, String>,

    // Timing & telemetry
    pub turn_start_time: Instant,
    pub token_budget: TokenBudget,

    // Journal (replaces item_sink callback)
    pub journal: Arc<dyn TurnJournal>,

    // Reasoning replay (currently passed separately)
    pub reasoning_to_replay: Option<Vec<ReasoningReplay>>,
}
```

**Key change**: `RunTurnRequest` and `ResumeTurnRequest` merge into `TurnContext` construction. The agent no longer needs separate parameters for provider, conversation_history, reasoning_to_replay, item_sink — they're all in the context.

**Files to modify**:
- `crates/deepomni-agent/src/lib.rs`: Replace `TurnExecutionContext` + `RunTurnRequest` + `ResumeTurnRequest` with unified `TurnContext`
- `crates/deepomni-runtime/src/lib.rs`: Build `TurnContext` in `submit_turn` and `approve_tool`

**Verification**: `cargo test --workspace`

### Stage 9: `deepomni-engine` crate

**Duration**: ~90 min

Create new crate at L3:

```
crates/deepomni-engine/
├── Cargo.toml
└── src/
    ├── lib.rs              # re-exports
    ├── session.rs          # SessionManager (moved from Runtime)
    ├── turn_fsm.rs         # TurnStateMachine
    ├── request_compiler.rs # RequestCompiler
    ├── tool_transaction.rs # ToolTransactionEngine
    └── approval.rs         # ApprovalCoordinator
```

Move from `deepomni-runtime` into `deepomni-engine`:
- Session/thread lifecycle management
- Turn FSM (the logic currently in `submit_turn`)
- Approval state machine (`resolve_pending_approval`, `approve_tool`, `reject_tool`)
- PendingApproval management

`deepomni-runtime` becomes a thin facade over `deepomni-engine`, handling only SDK-facing concerns (parameter validation, config resolution, provider registry).

**Files to create**:
- `crates/deepomni-engine/Cargo.toml`
- `crates/deepomni-engine/src/lib.rs`
- `crates/deepomni-engine/src/session.rs`
- `crates/deepomni-engine/src/turn_fsm.rs`
- `crates/deepomni-engine/src/request_compiler.rs`
- `crates/deepomni-engine/src/tool_transaction.rs`
- `crates/deepomni-engine/src/approval.rs`

**Files to modify**:
- `Cargo.toml`: add `deepomni-engine` to workspace
- `crates/deepomni-runtime/src/lib.rs`: delegate to engine

**Verification**: `cargo test --workspace` passes.

---

## Phase B: Architecture Upgrade (from ref/codex + ref/claude-code)

Phase B brings the proven architectural patterns from Codex and Claude Code into DeepOmni. The stages are independent where possible, but the recommended order is ContextManager → TurnContext → engine/services → approval cache → parallel tools so transcript ownership is settled before the larger runtime decomposition.

### Stage 10: SessionServices Container (from Codex `SessionServices`)

**Priority**: High — prerequisite for Runtime decomposition.

**Problem**: `RuntimeInner` (1039 lines) is a god-object mixing 15+ concerns. Codex separates into `Session` (identity + state) + `SessionServices` (20+ injected services) + `SessionState` (mutable per-session state + ContextManager).

**Design**: Extract a `SessionServices` struct:

```rust
// crates/deepomni-runtime/src/services.rs
pub struct SessionServices {
    pub state: Arc<StateStore>,
    pub tool_registry: Arc<ToolRegistry>,
    pub policy_engine: Arc<PolicyEngine>,
    pub provider_registry: Arc<RwLock<ProviderRegistry>>,
    pub skill_manager: Arc<RwLock<SkillManager>>,
    pub plugin_manager: Arc<RwLock<PluginManager>>,
    pub hook_dispatcher: Arc<HookDispatcher>,
    pub event_bus: Arc<EventBus>,         // or Arc<dyn TurnJournal> after Phase A
}

// RuntimeInner becomes thin:
struct RuntimeInner {
    config: ResolvedConfig,
    services: Arc<SessionServices>,
    active_sessions: RwLock<HashMap<ThreadId, ActiveSession>>,
}

struct ActiveSession {
    thread: Thread,
    context_manager: ContextManager,       // from Stage 8
    active_turn_id: Option<TurnId>,
    approval_store: ApprovalStore,          // from Stage 11
}
```

**Key change**: `TurnRunner::new()` takes `Arc<SessionServices>` instead of individual `Arc<ToolRegistry>` + `Arc<PolicyEngine>` + `Arc<EventBus>`. Adding a new service no longer requires changing TurnRunner's constructor.

**Files to modify**:
- `crates/deepomni-runtime/src/lib.rs`: Extract `SessionServices`, refactor `RuntimeInner`

**Verification**: `cargo test --workspace`

### Stage 11: ApprovalStore Session Cache (from Codex `ApprovalStore`)

**Priority**: High — critical UX improvement.

**Problem**: Every tool call requires re-approval even if the same tool+args pattern was already approved this session. Codex caches `ReviewDecision` per serialized approval key with `with_cached_approval()`.

**Design**:

```rust
// crates/deepomni-tools/src/approval_store.rs
pub struct ApprovalStore {
    cache: HashMap<String, ApprovalDecision>,
}

impl ApprovalStore {
    pub fn get<K: Serialize>(&self, key: &K) -> Option<ApprovalDecision>;
    pub fn put<K: Serialize>(&mut self, key: K, decision: ApprovalDecision);

    /// Check cache first; if miss, call fetch() and cache the result.
    pub async fn with_cached_approval<K, F, Fut>(
        &self, keys: Vec<K>, fetch: F
    ) -> ApprovalDecision
    where K: Serialize, F: FnOnce() -> Fut, Fut: Future<Output = ApprovalDecision>;
}
```

**Key change**: `ToolOrchestrator::evaluate()` checks `ApprovalStore` before returning `NeedsApproval`. Session-scoped: cleared when session ends, persisted approval still goes to `pending_approvals` table for cross-restart resume.

**Cache key constraints**: approval cache keys must include the tool name, canonicalized JSON arguments, cwd/workspace, approval policy/agent mode, and tool capability/mutability version. Never cache only by tool name or raw args string.

**Files to modify**:
- `crates/deepomni-tools/src/lib.rs`: Add `ApprovalStore`
- `crates/deepomni-tools/src/orchestrator.rs`: Integrate `with_cached_approval`
- `crates/deepomni-runtime/src/lib.rs`: Hold `ApprovalStore` per `ActiveSession`

**Verification**: `cargo test --workspace`, new test: approve once → same tool auto-proceeds.

### Stage 12: Parallel Tool Execution (from Codex `ToolCallRuntime` + Claude Code `StreamingToolExecutor`)

**Priority**: High — significant performance improvement for multi-tool turns.

**Problem**: DeepOmni executes tools strictly serially. Both Codex and Claude Code support parallel execution of non-mutating tools.

**Design**:

```rust
// crates/deepomni-tools/src/parallel.rs
pub struct ToolCallRuntime {
    registry: Arc<ToolRegistry>,
    /// Read lock = parallel (non-mutating), write lock = exclusive (mutating).
    parallel_lock: Arc<RwLock<()>>,
}

impl ToolCallRuntime {
    /// Execute multiple tool calls, respecting parallel/exclusive semantics.
    pub async fn execute_batch(
        &self,
        calls: Vec<PendingToolCall>,
        ctx: &TurnContext,
    ) -> Vec<ToolCallResult> {
        let (parallel, exclusive): (Vec<_>, Vec<_>) =
            calls.into_iter().partition(|c| c.supports_parallel && !c.is_mutating);

        // Run all non-mutating tools in parallel under read lock.
        let parallel_results = futures::future::join_all(
            parallel.into_iter().map(|call| {
                let lock = self.parallel_lock.clone();
                async move {
                    let _guard = lock.read().await;
                    self.execute_single(call, ctx).await
                }
            })
        ).await;

        // Run mutating tools sequentially under write lock.
        let mut exclusive_results = Vec::new();
        for call in exclusive {
            let _guard = self.parallel_lock.write().await;
            exclusive_results.push(self.execute_single(call, ctx).await);
        }

        // Return results in the original tool-call order, not completion order,
        // so the model transcript stays deterministic.
        merge_in_original_order(parallel_results, exclusive_results)
    }
}
```

**Key change**: The agent loop's tool execution section (`ToolCallComplete` handler in `run_turn`) collects all tool calls from one streaming iteration, then dispatches via `ToolCallRuntime::execute_batch()` instead of executing one-by-one.

**Files to modify**:
- `crates/deepomni-tools/src/lib.rs`: Add `ToolCallRuntime`
- `crates/deepomni-agent/src/lib.rs`: Batch tool calls through `ToolCallRuntime`

**Verification**: `cargo test --workspace`, new test: two non-mutating tools execute concurrently.

---

## Architecture Gap Matrix

| Codex/CC Architecture Pattern | Plan Stage | Priority | Status |
|-------------------------------|-----------|----------|--------|
| Single event data source | Stage 1–4 (Journal) | High | Phase A |
| Session/Turn separation | Stage 9 (Engine) + Stage 8 (TurnContext) | Extreme | Phase A+B |
| RequestCompiler 3 paths | Stage 6 | Medium | Phase A |
| ContextManager (token-aware history) | **Stage 7** | **High** | Phase B |
| TurnContext fat snapshot | **Stage 8** | **Extreme** | Phase B |
| SessionServices container | **Stage 10** | **High** | Phase B |
| ApprovalStore session cache | **Stage 11** | **High** | Phase B |
| Parallel tool execution | **Stage 12** | **High** | Phase B |
| StateStore trait abstraction | Future | Medium | Backlog |
| Multi-agent Mailbox communication | Future | Medium | Backlog |
| ContextualUserFragment protocol | Future | Medium | Backlog |

## Verification Checklist

After each stage:
```bash
cargo build --workspace          # must compile
cargo test --workspace           # all tests pass
cargo clippy --workspace --all-targets -- -D warnings  # gate check
```

Final integration check:
```bash
# SSE replay test
# Approval resume test (create turn → NeedsApproval → approve → turn completes)
# Multi-round tool continuation test
# Parallel tool execution test
cargo test --workspace
```

## Rollback Safety

Each stage is independently compilable and testable. No stage depends on a future stage. If Stage N breaks tests, we can stop and fix without undoing Stage N-1.

Phase A: The old `events` and `turn_items` tables are kept during Stages 1-4. They are only deprecated (not dropped) until Stage 5 validates the journal-based paths. The `pending_approvals` table remains as a materialized view even after Stage 6.

Phase B: Each stage adds new capability without removing existing code. Stage 7 (ContextManager) wraps existing `Vec<ModelMessage>` without changing the underlying data. Stage 8 (TurnContext) can be done incrementally by adding fields. Stage 10 (SessionServices) is a pure refactor. Stages 11-12 add new modules without touching existing approval/tool paths until verified.
