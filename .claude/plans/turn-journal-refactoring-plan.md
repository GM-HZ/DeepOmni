# TurnJournal Architecture Refactoring Plan

## Context

After 22 iterations of development, DeepOmni has 24 crates, 167 tests, and a working agent loop. However, three persistent problems remain:

1. **Three separate stores for turn events**: `events` table (EventBus broadcast), `turn_items` table (TurnItemStore), and `pending_approvals` table all write independently with no transaction boundary between them.
2. **TurnRunner directly depends on EventBus**: The agent crate has an `Arc<EventBus>` field and calls `emit()` directly with placeholder seq=0. This couples the agent to a specific broadcast implementation and makes seq tracking ad-hoc.
3. **Request builder reuses new-turn logic for continuations**: `build_request_messages_from_assembled` uses `history.is_empty()` to decide between new-turn and continuation paths, causing message ordering bugs.

The root cause: **no single source of truth for what happened in a turn**. 

## Goal

Introduce a `TurnJournal` trait that becomes the single append-only record of everything that happens in a turn. `EventBus`, `TurnItemStore`, and `PendingApprovalStore` become projections of this journal.

```
Before:                          After:
┌──────────┐                    ┌──────────────┐
│ EventBus │──── broadcast      │  TurnJournal │── append ──→ journal_entries table
├──────────┤                    │              │── replay ──→ reconstruct transcript  
│TurnItems │──── persistence    │              │── subscribe → live projection (SSE)
├──────────┤                    └──────┬───────┘
│PendingAp │──── approval state        │
└──────────┘                    ┌──────┴───────┐
                                │  Projections │
                                ├──────────────┤
                                │ EventBus     │ (materialized broadcast)
                                │ PendingStore │ (materialized read model)
                                │ SSE replay   │ (query projection)
                                └──────────────┘
```

## Design Decisions (from review)

1. **JournalEntry ≠ EventFrame**: JournalEntry is the internal complete record. EventFrame stays as the public, stable, host-safe projection. TurnItem is deprecated.
2. **pending_approvals table stays**: As a materialized read model, updated synchronously from journal writes within the same transaction.
3. **TurnJournal trait lives in new `deepomni-journal` crate** at L1, not in protocol. StateStore implements it.
4. **New crate naming**: `deepomni-engine` for Session/Turn FSM, not `deepomni-core-runtime` (avoids confusion with existing `deepomni-core`).
5. **RequestCompiler gets three explicit paths**, not one function with `history.is_empty()` branching.
6. **EventFrame remains**: SSE/SDK clients get the stable projection. Journal is the engine's internal contract.

## Target Crate Hierarchy

```
L0 foundation:
  deepomni-core          error/result/redact
  deepomni-protocol      wire types, EventFrame, JournalEntry, IDs
  deepomni-config        config types

L1 primitives:
  deepomni-state         StateStore (implements TurnJournal trait)
  deepomni-journal       TurnJournal trait, JournalEntry, JournalSubscriber (NEW)
  deepomni-events        EventBus (projection from journal)
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
    ToolCallApproved { call_id: ToolCallId },
    ToolCallRejected { call_id: ToolCallId },
    ToolCallCompleted { call_id: ToolCallId, success: bool, output_preview: Option<String> },
    
    // Approval
    ApprovalPending { call_id: ToolCallId, approval_id: String, reason: String },
    ApprovalResolved { approval_id: String, approved: bool },
    
    // Metadata
    ContextCompaction { reason: String },
    ProviderUsage { prompt_tokens: u64, completion_tokens: u64 },
}

/// Trait for append-only turn journal. StateStore implements this.
pub trait TurnJournal: Send + Sync {
    /// Append a journal entry. Returns the assigned monotonic sequence number.
    fn append(&self, thread_id: &str, turn_id: &str, entry: &JournalEntry) -> Result<i64>;
    
    /// Replay entries since a given sequence (exclusive).
    fn replay(&self, thread_id: &str, since_seq: i64) -> Result<Vec<(i64, JournalEntry)>>;
    
    /// Subscribe to live journal entries.
    fn subscribe(&self, thread_id: ThreadId) -> JournalSubscriber;
}

pub struct JournalSubscriber { /* wraps broadcast::Receiver<JournalEntry> */ }
```

**Files to create**:
- `crates/deepomni-journal/Cargo.toml`
- `crates/deepomni-journal/src/lib.rs`

**Files to modify**:
- `Cargo.toml` (workspace members): add `deepomni-journal`
- `crates/deepomni-protocol/src/lib.rs`: re-export `JournalEntry` types if needed

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
    entry_type TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(thread_id, seq)
);
```

Implement `TurnJournal` trait on `StateStore`:
- `append()`: BEGIN IMMEDIATE, compute max seq+1, insert, COMMIT. If entry is `ApprovalPending`, also insert into `pending_approvals` read model. If entry is `ApprovalResolved`, also update `pending_approvals` status. ALL in one transaction.
- `replay()`: SELECT from journal_entries WHERE thread_id = ? AND seq > ? ORDER BY seq.
- `subscribe()`: delegates to EventBus broadcast channel (or creates a new broadcast for journal entries).

**Files to modify**:
- `crates/deepomni-state/Cargo.toml`: add `deepomni-journal` dep
- `crates/deepomni-state/src/lib.rs`: add `journal_entries` table, impl `TurnJournal`
- `crates/deepomni-journal/src/lib.rs`: define `JournalProjection` helper

**Verification**: `cargo test -p deepomni-state` — new test: append JournalEntry, replay, verify seq monotonicity.

### Stage 3: TurnRunner depends on TurnJournal, not EventBus

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

The private `emit()` helper changes from `event_bus.emit(..., event)` to `journal.append(..., JournalEntry::ToolCallCompleted {...})`. All 18+ call sites in `run_turn` and `continue_after_approval` convert from `EventFrame` variants to `JournalEntry` variants.

The `item_sink` callback on `TurnExecutionContext` is deprecated — `ctx.record(TurnItem::...)` calls are replaced with `journal.append(JournalEntry::...)`.

**Files to modify**:
- `crates/deepomni-agent/Cargo.toml`: replace `deepomni-events` with `deepomni-journal`
- `crates/deepomni-agent/src/lib.rs`: TurnRunner field change, emit → journal.append
- `crates/deepomni-agent/tests/full_turn.rs`: update test setup
- `crates/deepomni-agent/tests/subagent.rs`: update test setup
- `crates/deepomni-runtime/Cargo.toml`: add `deepomni-journal`
- `crates/deepomni-runtime/src/lib.rs`: RuntimeBuilder::build to inject journal into TurnRunner

**Verification**: `cargo test --workspace` passes (all 167 tests must still pass).

### Stage 4: EventBus becomes journal projection

**Duration**: ~60 min

`EventBus` no longer calls `StateStore::append_event` directly. Instead:

1. Runtime wires a `JournalProjection` that listens to journal entries and converts them to `EventFrame` for broadcast.
2. SSE replay reads from `journal.replay()` instead of `state.get_events_since()`.
3. The `persist_fn` callback on EventBus is removed — journal entries are the source of truth.

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

### Stage 5: RequestCompiler — three explicit paths

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

### Stage 6: `deepomni-engine` crate

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
cargo test --workspace
```

## Rollback Safety

Each stage is independently compilable and testable. No stage depends on a future stage. If Stage N breaks tests, we can stop and fix without undoing Stage N-1.

The old `events` and `turn_items` tables are kept during Stages 1-4. They are only deprecated (not dropped) until Stage 5 validates the journal-based paths. The `pending_approvals` table remains as a materialized view even after Stage 6.
