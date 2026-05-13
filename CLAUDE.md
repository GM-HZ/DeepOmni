# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test

```bash
cargo build --workspace          # build all crates
cargo test --workspace           # run all tests (~167 tests, 56 suites)
cargo test -p <crate>            # run a single crate's tests
cargo test -p <crate> -- --list  # list tests in a crate
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Rust edition 2024, minimum version 1.88.

## Design Philosophy

DeepOmni targets the architectural standards of `ref/codex` (Rust workspace, ~113 crates). The reference implementation embodies patterns that separate industrial-grade agent runtimes from prototypes. Every change should move us toward these patterns, not away from them.

### Core Architectural Principles (from Codex)

**1. Session ≠ Turn ≠ Services.** The single most important separation in the system:
- **Session** — long-lived, per-conversation-thread. Owns identity, mutable state (behind a Mutex), and a DI container of shared services. Survives across many turns.
- **TurnContext** — frozen per-turn snapshot. Created fresh for each model interaction. Captures everything the agent needs for one sampling+tool loop: provider, model_info, approval_policy, permission_profile, cwd, environments, tools_config, features, timing state, journal handle. Passed as `Arc<TurnContext>`. Never mutated after construction.
- **SessionServices** — DI container holding all shared infrastructure (tool registry, policy engine, provider registry, hook dispatcher, MCP manager, skill manager, etc.). Constructed once, shared immutably across all turns. Adding a new service should never require changing TurnRunner's constructor.

**2. Trait abstraction over concrete implementations.** Storage, tools, providers, hooks — all core capabilities are defined as traits with at least two implementations (production + in-memory for tests). Never depend on a concrete `StateStore` struct; depend on a `ThreadStore` trait. This is what makes the system testable without SQLite.

**3. ContextManager is token-aware, not a Vec.** The conversation transcript is not raw `Vec<ModelMessage>` passed around. It lives in a `ContextManager` that tracks token usage from API responses, supports versioned compaction/rollback, and provides `for_prompt()` normalization. Token budgets are enforced at three trigger points: pre-turn (model downshift), pre-sampling (hard limit), mid-turn (after tool execution, before follow-up).

**4. Single source of truth for turn data.** Events, turn items, and approvals write to one append-only journal. Materialized views (like `pending_approvals`) are synchronous projections within the same transaction, never independent writes. The public `EventFrame` is a projection of the internal `JournalEntry` — external clients never see internal tool outputs or approval snapshots.

**5. Request paths are explicit, not branched.** Three separate methods (`compile_new_turn`, `compile_tool_continuation`, `compile_approval_resume`) instead of one function with `history.is_empty()` branching. Each path has guaranteed message ordering. No fake user messages injected to satisfy the API shape.

**6. Tool orchestration is a pipeline, not an if-statement.** The flow is: approve (check cache → policy → user) → select sandbox → execute → retry with escalated sandbox on denial. Approval decisions are cached in a session-scoped `ApprovalStore` keyed by canonicalized (tool_name, args_json, cwd, policy, capability_version). Non-mutating tools execute in parallel under a shared read lock; mutating tools serialize under an exclusive write lock.

**7. Module discipline.** Target under 500 LoC per module (excluding tests). Extract new modules aggressively — especially from high-touch orchestration files. Prefer private modules with explicit public API exports. A 3,600-line `lib.rs` is a god object, not a module.

### Anti-Patterns (actively being removed)

| Anti-Pattern | Example | Fix |
|---|---|---|
| God-object runtime | `deepomni-runtime/src/lib.rs` at 3,615 lines with 77-field `RuntimeInner` | Extract into `SessionManager` + `TurnStateMachine` + `ApprovalCoordinator` in `deepomni-engine`; Runtime becomes a thin SDK facade |
| Anemic engine types | `SessionManager` (135 lines, just a HashMap wrapper) | Engine types should own the business logic, not delegate back to Runtime adapters |
| Concrete type dependencies | Code depends on `StateStore` struct directly | Introduce `ThreadStore` trait with `LocalThreadStore` + `InMemoryThreadStore` |
| Duplicated modules | `PromptBuilder` in both `deepomni-context` and `deepomni-prompt` | Consolidate; one crate owns prompt building |
| Skeleton implementations | `deepomni-sandbox` is pass-through only; `deepomni-mcp` tool execution is a stub | Fill in or delete the crate |
| Unconsulted state machines | `TurnStateMachine` exists but `TurnRunner` doesn't check it before advancing | State machines must gate transitions, not just document them |
| Unwired registries | `CapabilityRegistry` exists but nothing checks it before enabling features | Wire into RuntimeBuilder |

## Architecture

DeepOmni is a DeepSeek-native agent runtime for production coding agents. 27 crates in a Rust workspace, organized in 4 layers.

### L0 — Foundation (no domain logic, no runtime deps)

- **deepomni-core**: `DeepOmniError` enum + `Result<T>` alias + API key redaction. Must NOT accumulate domain logic. Keep under 100 lines.
- **deepomni-protocol**: Wire/serializable types only. IDs (ThreadId/TurnId/ToolCallId/MessageId/SubagentId/EventSeq), `EventFrame` (public projection enum), `EventEnvelope`, `TurnItem`, approval/tool/sandbox DTOs. Must NOT depend on any other deepomni crate. Must NOT contain internal types like `JournalEntry`.
- **deepomni-config**: 6-layer config precedence (builder > CLI > env > workspace > user > defaults). Provider registry types. Must NOT contain runtime logic.

### L1 — Domain Primitives (traits + implementations, no orchestration)

- **deepomni-state**: SQLite-backed `StateStore`. Implements `TurnJournal` trait. Tables: threads, turns, journal_entries, pending_approvals, events (legacy). Migration system. **Gap:** No `ThreadStore` trait abstraction — code depends on concrete struct.
- **deepomni-journal**: `TurnJournal` trait, `JournalEntry` enum (22 variants), `JournalRecord` envelope, `JournalSubscriber`, projection helpers. The single source of truth for all turn data. StateStore implements TurnJournal; InMemoryJournal exists for tests.
- **deepomni-events**: `EventBus` with per-thread broadcast channels. Public EventFrame broadcast only — persistence is delegated to the journal. SSE formatting helpers.
- **deepomni-policy**: `AgentMode` (Plan/Agent/Trusted), `ApprovalRequirement`, `PermissionProfile` with allowlist/denylist, `PolicyEngine` with session-scoped dynamic grants.
- **deepomni-sandbox**: Sandbox abstraction. **Critical gap:** All platform implementations are pass-through — no actual sandbox enforcement. macOS Seatbelt and Linux Landlock must be implemented.
- **deepomni-model-provider**: `ModelProvider` trait (provider_name, known_models, stream), `ModelDelta` enum, `ModelRequest`/`ModelMessage`, `ProviderRegistry`. **Gap:** No `Usage` return from trait — provider must expose usage separately.
- **deepomni-tools**: `ToolHandler` trait, `ToolRegistry`, `ToolOrchestrator` (approve→sandbox→execute→retry), `ApprovalStore` (session-scoped decision cache), `ToolRouter`, `ToolCallRuntime` (parallel-safe read lock / exclusive write lock).

### L2 — Capabilities (pluggable, feature-gated)

- **deepomni-capabilities**: `Capability` enum + `Stage` lifecycle + `CapabilityRegistry`. **Gap:** Registry exists but is not wired into RuntimeBuilder.
- **deepomni-context**: `ContextManager` (token-aware, version-tracked), `TokenBudget` (5-bucket allocation), `ContextFragment` with marker tags, `assemble_context()` with deterministic overflow order. **Gap:** Token estimation uses naive `chars/4` heuristic.
- **deepomni-prompt**: `PromptBuilder` with 5-layer priority and SHA-256 cache hash. **Gap:** Duplicated in `deepomni-context` — consolidate.
- **deepomni-provider-deepseek**: DeepSeekProvider via OpenAI-compatible Chat Completions. SSE streaming, reasoning replay, tool call state machine.
- **deepomni-tool-builtin**: 11 built-in tools implementing `ToolHandler`. Workspace-aware path resolution.
- **deepomni-skills**: SKILL.md discovery, frontmatter parsing, budget-aware context rendering.
- **deepomni-plugin**: `plugin.json` manifest loading, `PluginManager` with search paths and capability summaries.
- **deepomni-mcp**: MCP client with stdio transport, `McpManager`. **Gap:** `McpToolHandler.handle()` is a stub — never actually invokes tools on the MCP server.
- **deepomni-hooks**: 7 hook points, `HookDispatcher` with timeout isolation.

### L3 — Engine (orchestration, state machines, session lifecycle)

- **deepomni-engine**: `SessionManager`, `SessionLoop` (with OpHandler trait for single-entry-point dispatch), `TurnCoordinator`, `TurnStateMachine` (7 states, validated transitions), `RequestCompiler` (3 explicit paths), `ApprovalCoordinator`, `CompactionTracker`, `AgentControl` + `AgentRegistry`, `Mailbox` (inter-agent mpsc). **Gap:** Most engine types are thin coordinators — the real business logic still lives in `RuntimeTurnAdapter` inside the runtime crate.
- **deepomni-agent**: `TurnRunner` with continuation loop, `TurnContext` (frozen per-turn snapshot, 15 fields), `RunTurnRequest`/`ResumeTurnRequest`. The agent loop: context→stream→tool detection→orchestration→approval→execution→continuation. **Gap:** `run_turn()` is ~500 lines in one method; should be decomposed into pipeline stages.
- **deepomni-runtime**: SDK-facing facade. `submit_turn`, `approve_tool`, `reject_tool`. Wires ProviderRegistry, SkillManager, PluginManager, HookDispatcher. **Critical gap:** 3,615-line `lib.rs` — must be decomposed. Session lifecycle and turn FSM logic should move to `deepomni-engine`. Runtime should be a thin validation + delegation layer.

### L4 — Hosts

- **deepomni-server**: Axum HTTP/SSE server. 15 endpoints. Auth middleware. Subscribe-first SSE with high-water mark. **Gap:** All errors return 500; no differentiated error responses.
- **deepomni-cli**: Placeholder (2 lines).
- **deepomni-test-support**: `MockModelProvider`, `TempWorkspace`, `EventCollector`, `FakeToolHandler`.

## Key Design Patterns

### Event / Journal Flow

```
TurnRunner → journal.append(JournalEntry)
  → StateStore.append() (BEGIN IMMEDIATE, compute seq, insert, COMMIT)
  → if ApprovalPending: project to pending_approvals table (same transaction)
  → broadcast JournalRecord to journal subscribers
  → projection task: journal_to_event_frame() → EventBus broadcast (public EventFrame)
  → SSE subscribers receive EventFrame
```

JournalEntry is internal (full tool outputs, approval snapshots). EventFrame is the public, redacted projection. External clients never see internal data. Legacy `events` and `turn_items` tables are retained for compatibility but new data writes exclusively through the journal.

### Approval Flow

```
ToolOrchestrator::evaluate(handler, agent_mode)
  → check ApprovalStore cache (session-scoped, keyed by canonicalized params)
  → if miss: evaluate policy → OrchestratorDecision::Proceed/NeedsApproval/Forbidden
  → if NeedsApproval:
      → journal.append(ApprovalPending { resume_snapshot })
      → pending_approvals table populated synchronously (journal projection)
      → emit EventFrame::ApprovalRequired to SSE
  → user calls approve_tool:
      → resolve_pending_approval (ApprovalStore first, StateStore fallback)
      → ApprovalStore.put(decision) for session cache
      → journal.append(ApprovalResolved)
      → ToolTransactionEngine executes the tool
      → model loop continues with tool result in transcript
```

### Turn Lifecycle Ownership

- **Runtime** owns turn lifecycle boundaries: emits `TurnStarted`/`TurnCompleted`/`TurnFailed` via journal
- **Agent** (TurnRunner) owns the sampling+tool loop: emits deltas, tool calls, tool results via journal
- **Engine** owns session state transitions and cross-turn coordination
- No component writes to the journal without going through the TurnJournal trait

### Tool Orchestration Pipeline

```
1. ToolRouter: model output → PendingToolCall (name, args, handler)
2. ToolOrchestrator::evaluate → Proceed/NeedsApproval/Forbidden
3. ApprovalStore::with_cached_approval → session-cached decision or user prompt
4. SandboxManager::decide → SandboxAttempt (read-only / workspace-write / danger-full-access)
5. ToolCallRuntime::execute_batch → parallel (non-mutating, read lock) + serial (mutating, write lock)
6. On sandbox denial: retry with escalated sandbox (if escalate_on_deny)
7. Results merged in original tool-call order for deterministic transcript
```

### Transcript Management

- `ContextManager` owns the append-only transcript (`Vec<ModelMessage>`) with version tracking
- `record_items()` appends with truncation policy applied
- `for_prompt()` normalizes and filters for model consumption
- Compaction at three checkpoints: pre-turn, pre-sampling, post-tool-execution
- `replace_history()` for compaction with version bump
- Diff-based context injection: `reference_context_item` tracks baseline; only diffs sent on subsequent turns

## Reference Projects

- `ref/codex` — Codex Rust workspace (~113 crates). **Primary architectural reference.** Source of truth for: Session/TurnContext/SessionServices separation, ContextManager, ThreadStore trait, ApprovalStore, ToolOrchestrator pipeline, Guardian auto-review, multi-agent Mailbox, hook system, module discipline, lint configuration.
- `ref/DeepSeek-TUI` — DeepSeek-native TUI agent. Reference for: provider behavior, reasoning replay, turn loop patterns.
- `ref/claude-code-sourcemap` — Reverse-engineered Claude Code TypeScript. Reference for: query loop, prompt layering, tool permission middleware, streaming tool executor.

## Current Refactoring Status

The 12-stage plan (`.claude/plans/turn-journal-refactoring-plan.md`) is fully implemented in terms of features, but structural cleanup remains:

**Done (Stages 1-12):**
- TurnJournal as single source of truth for turn data
- Journal-to-EventFrame projection bridge
- TurnRunner depends on TurnJournal, not EventBus
- Legacy event/turn_item writes retired
- RequestCompiler with 3 explicit paths
- ContextManager with token awareness and compaction
- TurnContext as frozen per-turn snapshot
- SessionServices DI container extracted
- ApprovalStore session cache
- ToolCallRuntime with parallel execution
- deepomni-engine crate scaffolded

**Remaining structural work:**
- Decompose `deepomni-runtime/src/lib.rs` (3,615 lines → target under 500): move business logic into engine, leave only SDK-facing validation + delegation
- Introduce `ThreadStore` trait abstraction (currently concrete `StateStore` only)
- Consolidate duplicated `PromptBuilder` (in both `deepomni-context` and `deepomni-prompt`)
- Implement actual sandbox enforcement (macOS Seatbelt, Linux Landlock)
- Complete MCP tool execution (currently a stub)
- Wire `CapabilityRegistry` into RuntimeBuilder
- Wire `TurnStateMachine` as an actual gate on turn transitions
- Keep engine types from remaining anemic — move logic from RuntimeTurnAdapter into engine methods
