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

## Architecture

DeepOmni is a DeepSeek-native agent runtime for production coding agents. 27 crates in a Rust workspace, organized in 4 layers:

### L0 — Foundation
- **deepomni-core**: `DeepOmniError` enum, `Result<T>` alias, API key redaction. Must NOT accumulate domain logic.
- **deepomni-protocol**: Wire/serializable types only — IDs (ThreadId/TurnId/ToolCallId/MessageId/SubagentId/EventSeq), EventFrame (25 variants), EventEnvelope {seq, frame}, TurnItem, thread/turn/approval/tool DTOs. No runtime deps.
- **deepomni-config**: 6-layer config precedence (builder > CLI > env > workspace > user > defaults). Provider registry (DeepSeek/OpenAI/Ollama/etc).

### L1 — Domain Primitives
- **deepomni-state**: SQLite-backed StateStore. Tables: threads, turns, events, tool_calls, turn_items, pending_approvals, journal_entries (planned). Migration system.
- **deepomni-events**: EventBus with broadcast + durable persistence gate. Broadcasts EventEnvelope. SSE helpers (`event_tag()`, `to_sse_data()`).
- **deepomni-policy**: AgentMode (Plan/Agent/Trusted), ApprovalRequirement, PermissionProfile with allowlist/denylist, dynamic per-session grants.
- **deepomni-sandbox**: Platform sandbox abstraction (macOS Seatbelt / Linux Landlock / Windows placeholder).
- **deepomni-model-provider**: ModelProvider trait, ModelStream, ModelDelta (Text/Reasoning/ToolCallStart/ArgsDelta/Complete/End), ProviderRegistry.
- **deepomni-tools**: ToolHandler trait (name/spec/is_mutating/supports_parallel/handle), ToolRegistry, ToolOrchestrator with approve→sandbox→execute→retry, ToolInvocation with per-thread workspace.

### L2 — Capabilities
- **deepomni-capabilities**: Feature gate registry (Capability enum with Stage lifecycle).
- **deepomni-context**: TokenBudget (5-bucket allocation), ContextFragment with marker tags, FragmentRole, assemble_context().
- **deepomni-prompt**: 5-layer PromptBuilder (override → agent → custom → default → append) with SHA-256 cache hash.
- **deepomni-provider-deepseek**: DeepSeekProvider implementing ModelProvider via OpenAI-compatible Chat Completions. SSE streaming parser, reasoning replay, tool call state machine.
- **deepomni-tool-builtin**: 11 built-in tools (read/write/edit_file, grep, list_files, shell_exec, git_status/diff/apply, apply_patch, todo_update). register_all().
- **deepomni-skills/plugin/mcp/hooks**: SkillManager, PluginManager (manifest + discovery), McpManager (JSON-RPC stdio client), HookDispatcher (7 hook points).

### L3 — Engine
- **deepomni-agent**: TurnRunner with continuation loop, TurnExecutionContext (frozen per-turn snapshot), RunTurnRequest/ResumeTurnRequest, per-round transcript pairing. The agent loop: context→stream→tool detection→orchestration→approval→execution→continuation.
- **deepomni-runtime**: Runtime facade. submit_turn, approve_tool, reject_tool, resolve_pending_approval (memory→state fallback). Wires ProviderRegistry, SkillManager, PluginManager, HookDispatcher, TaskManager.

### L4 — Hosts
- **deepomni-server**: Axum HTTP/SSE server. 15 endpoints. Auth middleware (Bearer token). Subscribe-first SSE with high-water mark.
- **deepomni-cli**: Debug CLI skeleton.
- **deepomni-test-support**: MockModelProvider (implements ModelProvider), TempWorkspace, EventCollector assertions.

## Key Design Patterns

**Event flow**: `emit_event()` → EventBus persistence callback → StateStore.append_event() (atomic BEGIN IMMEDIATE/COMMIT). EventBus broadcasts EventEnvelope with authoritative persisted seq. Failure to persist → no broadcast.

**Approval flow**: submit_turn → NeedsApproval → PendingApproval persisted to pending_approvals table + in-memory cache. approve_tool → resolve_pending_approval (memory first, state fallback) → update status → continue_after_approval → TurnRunner executes tool → provider loop continues.

**Turn lifecycle ownership**: Only Runtime emits TurnStarted/TurnCompleted/TurnFailed. Agent emits deltas and tool events only. Runtime uses must_emit_event for critical events with ? propagation.

**Tool orchestration**: ToolOrchestrator::evaluate(handler, agent_mode) → OrchestratorDecision::Proceed/NeedsApproval/Forbidden. Sandbox escalation via escalate_on_deny flag.

**Transcript**: append-only Vec<ModelMessage> across continuation rounds. Per-round tool_calls + tool_results strictly paired. Build from TurnItemStore (get_turn_items) on approval resume.

## Reference Projects

`ref/DeepSeek-TUI` — DeepSeek-native TUI agent. Reference for provider behavior, reasoning replay, turn loop patterns.
`ref/codex` — Codex Rust workspace (~113 crates). Reference for Session/TurnContext separation, ToolOrchestrator, ApprovalStore, Feature gates.
`ref/claude-code-sourcemap` — Reverse-engineered Claude Code TypeScript. Reference for query loop, prompt layering, tool permission middleware.

## Current Architecture Debt (planned refactoring)

See `.claude/plans/turn-journal-refactoring-plan.md` for the 6-stage plan to converge EventBus + TurnItemStore + PendingApprovalStore into a single `TurnJournal` append-only journal with EventFrame as projection.
