# Multi-Agent Protocol and Trace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the foundations for durable multi-agent coordination, protocol translation, and rollout trace without coupling UI clients to runtime internals.

**Architecture:** Build after the Op/Event Session Loop plan. Model each durable agent as a thread/session with a mailbox; add a protocol adapter between server and runtime; add a no-op-by-default trace writer fed by journal/event boundaries.

**Tech Stack:** Rust 2024, Tokio `mpsc`/`watch`, existing `ThreadId`, `JournalEntry`, `EventFrame`, `deepomni-engine`, `deepomni-runtime`, `deepomni-server`.

---

## Source Documents

- `docs/04.md`
- `docs/01.md` sections on App Server and event boundary

## Dependencies

This plan should start after:

1. `2026-05-11-op-event-session-loop.md` Task 4 is complete.
2. `2026-05-11-context-manager-compaction.md` Task 2 is complete.

## File Structure

- Create `crates/deepomni-protocol/src/agent.rs`: `AgentPath`, `InterAgentMessage`, `AgentInfo`.
- Create `crates/deepomni-engine/src/mailbox.rs`: mailbox sender/receiver.
- Create `crates/deepomni-engine/src/agent_control.rs`: registry and lifecycle manager.
- Create `crates/deepomni-server/src/protocol_adapter.rs`: server notification/request translation.
- Create `crates/deepomni-trace` or `crates/deepomni-engine/src/trace.rs`: trace writer trait and no-op implementation.
- Modify `Cargo.toml`: add `deepomni-trace` only if a dedicated crate is chosen.
- Modify `crates/deepomni-runtime/src/lib.rs`: wire `AgentControl`, protocol adapter entry points, and trace writer.

## Task 1: Inter-Agent Protocol Types

**Files:**
- Create: `crates/deepomni-protocol/src/agent.rs`
- Modify: `crates/deepomni-protocol/src/lib.rs`

- [ ] **Step 1: Write failing protocol tests**

```rust
#[test]
fn inter_agent_message_serializes_with_trigger_turn() {
    let message = InterAgentMessage {
        author: AgentPath::root(),
        recipient: AgentPath::from_string("/root/worker"),
        other_recipients: Vec::new(),
        content: "done".into(),
        trigger_turn: true,
    };
    let json = serde_json::to_value(message).unwrap();
    assert_eq!(json["trigger_turn"], true);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-protocol inter_agent_message_serializes_with_trigger_turn`

Expected: FAIL.

- [ ] **Step 3: Implement types**

Implement:

- `AgentPath`
- `InterAgentMessage`
- `AgentInfo`
- `AgentStatus`

- [ ] **Step 4: Run protocol tests**

Run: `cargo test -p deepomni-protocol`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-protocol/src/agent.rs crates/deepomni-protocol/src/lib.rs
git commit -m "feat(protocol): add inter-agent message types"
```

## Task 2: Mailbox

**Files:**
- Create: `crates/deepomni-engine/src/mailbox.rs`
- Modify: `crates/deepomni-engine/src/lib.rs`

- [ ] **Step 1: Write failing mailbox tests**

Test:

- send assigns increasing sequence numbers.
- receiver drains pending messages.
- `has_pending_trigger_turn()` returns true when any message has `trigger_turn`.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-engine mailbox`

Expected: FAIL.

- [ ] **Step 3: Implement mailbox**

Use:

```rust
tokio::sync::mpsc::UnboundedSender<InterAgentMessage>
tokio::sync::watch::Sender<u64>
AtomicU64
VecDeque<InterAgentMessage>
```

- [ ] **Step 4: Run engine tests**

Run: `cargo test -p deepomni-engine`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-engine/src/mailbox.rs crates/deepomni-engine/src/lib.rs
git commit -m "feat(engine): add agent mailbox"
```

## Task 3: Agent Registry and Control

**Files:**
- Create: `crates/deepomni-engine/src/agent_control.rs`
- Modify: `crates/deepomni-engine/src/lib.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing registry tests**

Test:

- registry allocates unique paths.
- registry enforces max agent count.
- spawn metadata records parent thread and depth.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-engine agent_registry`

Expected: FAIL.

- [ ] **Step 3: Implement registry**

Implement:

- `AgentRegistry`
- `AgentMetadata`
- `SpawnAgentOptions`
- max depth and max total limits

- [ ] **Step 4: Implement `AgentControl` shell**

`AgentControl` should hold a weak runtime/manager reference but tests can use an in-memory fake spawner.

- [ ] **Step 5: Run engine tests**

Run: `cargo test -p deepomni-engine`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-engine/src/agent_control.rs crates/deepomni-engine/src/lib.rs
git commit -m "feat(engine): add agent registry and control"
```

## Task 4: Spawn Subagent Through AgentControl

**Files:**
- Modify: `crates/deepomni-agent/src/lib.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`
- Modify: `crates/deepomni-engine/src/agent_control.rs`

- [ ] **Step 1: Write failing integration test**

Existing `spawn_subagent` should create an independent thread/session and register it in `AgentRegistry`.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-runtime spawn_subagent_registers_agent`

Expected: FAIL.

- [ ] **Step 3: Route spawn through `AgentControl`**

Do not run child by recursive `run_turn()` directly. Create child session/thread and submit initial op.

- [ ] **Step 4: Add result communication**

Child completion sends `InterAgentMessage { trigger_turn: true }` to parent mailbox.

- [ ] **Step 5: Run agent/runtime tests**

Run:

```bash
cargo test -p deepomni-agent
cargo test -p deepomni-runtime
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-agent crates/deepomni-runtime crates/deepomni-engine
git commit -m "feat(agent): spawn subagents through agent control"
```

## Task 5: Protocol Adapter

**Files:**
- Create: `crates/deepomni-server/src/protocol_adapter.rs`
- Modify: `crates/deepomni-server/src/lib.rs`
- Modify: `crates/deepomni-protocol/src/event.rs` if new server events are needed.

- [ ] **Step 1: Write failing adapter tests**

Test:

- `EventFrame::ToolCallRequiresApproval` becomes `ServerRequest::ApprovalNeeded`.
- `EventFrame::AssistantMessageDelta` becomes `ServerNotification::AssistantDelta`.
- client approval response becomes `Op::ApprovalDecision`.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-server protocol_adapter`

Expected: FAIL.

- [ ] **Step 3: Implement adapter enums if needed**

Prefer adding protocol types in `deepomni-protocol`:

```rust
pub enum ServerNotification { ... }
pub enum ServerRequest { ... }
pub enum ClientResponse { ... }
```

- [ ] **Step 4: Implement translation**

Keep adapter pure and easy to test:

```rust
pub fn event_to_server_message(event: EventFrame) -> Option<ServerMessage>;
pub fn client_response_to_op(response: ClientResponse) -> Result<Op, ProtocolError>;
```

- [ ] **Step 5: Route SSE/HTTP through adapter**

Server handlers should not directly know runtime internals beyond `submit(Op)` and event subscription.

- [ ] **Step 6: Run server tests**

Run: `cargo test -p deepomni-server`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/deepomni-server crates/deepomni-protocol
git commit -m "feat(server): add protocol adapter layer"
```

## Task 6: Trace Writer Trait

**Files:**
- Create: `crates/deepomni-trace/Cargo.toml`
- Create: `crates/deepomni-trace/src/lib.rs`
- Modify: `Cargo.toml`
- Modify: `crates/deepomni-runtime/Cargo.toml`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing trace tests**

Test:

- `NoopTraceWriter` accepts all calls.
- `MemoryTraceWriter` records turn/tool/agent events in order.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-trace`

Expected: FAIL because crate is missing.

- [ ] **Step 3: Implement trace crate**

Implement:

```rust
pub trait TraceWriter: Send + Sync {
    fn record_turn_started(&self, thread_id: &ThreadId, turn_id: &TurnId);
    fn record_inference_attempt(&self, thread_id: &ThreadId, turn_id: &TurnId, model: &str);
    fn record_tool_dispatch(&self, thread_id: &ThreadId, turn_id: &TurnId, tool_name: &str);
    fn record_compaction(&self, thread_id: &ThreadId, before_tokens: u64, after_tokens: u64);
    fn record_agent_spawn(&self, parent_thread_id: &ThreadId, child_thread_id: &ThreadId);
}
```

- [ ] **Step 4: Wire default no-op trace writer**

Add to `SessionServices`:

```rust
pub trace_writer: Arc<dyn TraceWriter>
```

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p deepomni-trace
cargo test -p deepomni-runtime
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/deepomni-trace crates/deepomni-runtime
git commit -m "feat(trace): add no-op trace writer boundary"
```

## Task 7: Journal-Backed Trace Events

**Files:**
- Modify: `crates/deepomni-runtime/src/lib.rs`
- Modify: `crates/deepomni-agent/src/lib.rs`
- Modify: `crates/deepomni-engine/src/session_loop.rs`

- [ ] **Step 1: Write failing trace integration test**

With `MemoryTraceWriter`, submit one turn with one tool call and assert trace captures turn start, inference attempt, and tool dispatch.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-runtime trace_records_turn_and_tool`

Expected: FAIL.

- [ ] **Step 3: Call trace writer from stable boundaries**

Prefer journal append/projection boundaries:

- turn start
- provider request start
- tool dispatch start
- compaction start/completion
- agent spawn

- [ ] **Step 4: Run workspace tests**

Run: `cargo test --workspace`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-runtime crates/deepomni-agent crates/deepomni-engine
git commit -m "feat(trace): record runtime trace checkpoints"
```

## Final Verification

- [ ] Run:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] Review:

```bash
git diff --check
```

