# Tool Pipeline Router and Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve DeepOmni's tool system into a layered pipeline: model-visible specs, router normalization, parallel runtime, registry dispatch, orchestrator approval/sandbox, and handlers.

**Architecture:** `deepomni-tools` owns tool routing and execution primitives; `deepomni-agent` consumes only normalized `ToolCall`s. Existing `ToolRegistry`, `ToolOrchestrator`, `ApprovalStore`, and `ToolCallRuntime` remain but become part of a clearer pipeline.

**Tech Stack:** Rust 2024, Tokio, `serde_json`, existing `deepomni-tools`, `deepomni-agent`, `deepomni-mcp`, `deepomni-hooks`, `deepomni-sandbox`.

---

## Source Documents

- `docs/02.md`
- `docs/01.md` section on model output and tool follow-up

## File Structure

- Create `crates/deepomni-tools/src/router.rs`: `ToolRouter`, `ToolCall`, source-specific tool metadata.
- Create `crates/deepomni-tools/src/runtime.rs`: move or re-export `ToolCallRuntime`, `PendingToolCall`, `ToolCallResult`.
- Create `crates/deepomni-tools/src/approval_store.rs`: move or re-export approval cache.
- Modify `crates/deepomni-tools/src/lib.rs`: module exports.
- Modify `crates/deepomni-agent/src/lib.rs`: collect tool calls per model step and execute through `ToolCallRuntime`.
- Modify `crates/deepomni-mcp/src/lib.rs`: register MCP tool mappings through router once available.
- Add tests in `deepomni-tools`, `deepomni-agent`, and later `deepomni-mcp`.

## Task 1: Split Tool Runtime Modules Without Behavior Change

**Files:**
- Create: `crates/deepomni-tools/src/approval_store.rs`
- Create: `crates/deepomni-tools/src/runtime.rs`
- Modify: `crates/deepomni-tools/src/lib.rs`

- [ ] **Step 1: Write no-regression tests**

Keep existing tests:

```bash
cargo test -p deepomni-tools approval_store_caches_decision_for_all_keys
cargo test -p deepomni-tools tool_call_runtime_runs_parallel_safe_tools_concurrently
```

- [ ] **Step 2: Move code into focused modules**

Move:

- `ApprovalDecision`, `ApprovalStore` → `approval_store.rs`
- `ToolCallRuntime`, `PendingToolCall`, `ToolCallResult` → `runtime.rs`

Re-export from `lib.rs`:

```rust
pub use approval_store::{ApprovalDecision, ApprovalStore};
pub use runtime::{PendingToolCall, ToolCallResult, ToolCallRuntime};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p deepomni-tools`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/deepomni-tools/src
git commit -m "refactor(tools): split approval store and runtime modules"
```

## Task 2: Add ToolRouter

**Files:**
- Create: `crates/deepomni-tools/src/router.rs`
- Modify: `crates/deepomni-tools/src/lib.rs`

- [ ] **Step 1: Write failing router tests**

```rust
#[test]
fn router_builds_function_tool_call_from_model_output() {
    let registry = Arc::new(ToolRegistry::new());
    let router = ToolRouter::new(registry);
    let call = router.build_function_call(
        ToolCallId::from_string("call-1"),
        "read_file",
        serde_json::json!({"path": "README.md"}),
    );
    assert_eq!(call.tool_name, "read_file");
    assert!(matches!(call.payload, ToolPayload::Function { .. }));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-tools router_builds_function_tool_call_from_model_output`

Expected: FAIL because `ToolRouter` is missing.

- [ ] **Step 3: Implement router types**

Implement:

```rust
pub struct ToolRouter {
    registry: Arc<ToolRegistry>,
    model_visible_specs: Vec<ToolSpec>,
    mcp_tool_mappings: HashMap<String, McpToolInfo>,
}

pub struct ToolCall {
    pub call_id: ToolCallId,
    pub tool_name: String,
    pub payload: ToolPayload,
    pub supports_parallel: bool,
    pub is_mutating: bool,
}
```

- [ ] **Step 4: Add spec visibility tests**

Test `model_visible_specs()` returns only specs from registered handlers initially.

- [ ] **Step 5: Run tool tests**

Run: `cargo test -p deepomni-tools`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-tools/src/router.rs crates/deepomni-tools/src/lib.rs
git commit -m "feat(tools): add tool router"
```

## Task 3: Agent Uses Router for Tool Calls

**Files:**
- Modify: `crates/deepomni-agent/src/lib.rs`
- Modify: `crates/deepomni-agent/tests/full_turn.rs`

- [ ] **Step 1: Write failing agent test**

Add a test ensuring unknown model tool output becomes a router-level forbidden result, not a direct registry lookup panic or silent skip.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-agent unknown_tool_is_rejected_by_router`

Expected: FAIL.

- [ ] **Step 3: Add `ToolRouter` to `TurnRunner`**

Change `TurnRunner` construction to create a router from the registry. Keep constructor signature stable if possible:

```rust
let router = ToolRouter::new(tool_registry.clone());
```

- [ ] **Step 4: Replace direct tool-name reconstruction**

When `ModelDelta::ToolCallComplete` arrives, call router to produce `ToolCall`.

- [ ] **Step 5: Run agent tests**

Run: `cargo test -p deepomni-agent`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-agent/src/lib.rs crates/deepomni-agent/tests/full_turn.rs
git commit -m "feat(agent): normalize model tool calls through router"
```

## Task 4: Batch Tool Execution in Agent Loop

**Files:**
- Modify: `crates/deepomni-agent/src/lib.rs`
- Add or modify: `crates/deepomni-agent/tests/full_turn.rs`

- [ ] **Step 1: Write failing concurrency test**

Create two read-only tools that sleep for 50ms and expose `supports_parallel() -> true`. Provider returns two tool calls in one model step. Assert max concurrent executions reaches 2.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-agent parallel_tool_calls_execute_in_batch`

Expected: FAIL because agent still executes tool calls one-by-one.

- [ ] **Step 3: Collect completed tool calls for one model step**

Inside the streaming loop, push normalized calls into `Vec<ToolCall>` instead of immediately executing them.

- [ ] **Step 4: Execute via `ToolCallRuntime::execute_batch()`**

After stream end for the model sampling step:

```rust
let results = self.tool_call_runtime.execute_batch(pending_calls).await;
```

Preserve result order by original call position.

- [ ] **Step 5: Keep approval short-circuit semantics**

If any mutating call needs approval, return `TurnResult::NeedsApproval` before executing later mutating calls. Read-only calls may execute only if all pending approvals are resolved or policy says proceed.

- [ ] **Step 6: Run agent tests**

Run: `cargo test -p deepomni-agent`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/deepomni-agent/src/lib.rs crates/deepomni-agent/tests/full_turn.rs
git commit -m "feat(agent): execute parallel-safe tool calls in batches"
```

## Task 5: Sandbox Attempt Execution

**Files:**
- Modify: `crates/deepomni-tools/src/runtime.rs`
- Modify: `crates/deepomni-sandbox/src/lib.rs`
- Modify: `crates/deepomni-agent/src/lib.rs`

- [ ] **Step 1: Write failing sandbox retry test**

Test `Proceed { sandbox_required: false, escalate_on_deny: true }` can retry unsandboxed when sandbox denial is classified.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-tools sandbox_denial_can_escalate`

Expected: FAIL.

- [ ] **Step 3: Add sandbox attempt abstraction**

Implement:

```rust
pub enum SandboxAttempt {
    Required,
    PreferredWithEscalation,
    Disabled,
}
```

- [ ] **Step 4: Route `OrchestratorDecision::Proceed` into runtime**

Pass sandbox strategy into `PendingToolCall`.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p deepomni-tools
cargo test -p deepomni-sandbox
cargo test -p deepomni-agent
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-tools crates/deepomni-sandbox crates/deepomni-agent
git commit -m "feat(tools): add sandbox attempt strategy"
```

## Task 6: Hook Payload Integration

**Files:**
- Modify: `crates/deepomni-tools/src/lib.rs`
- Modify: `crates/deepomni-hooks/src/lib.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing pre-hook block test**

Test a configured pre-tool hook can block a mutating tool before handler execution.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-hooks pre_tool_hook_blocks_execution`

Expected: FAIL.

- [ ] **Step 3: Add hook payload builders to `ToolHandler`**

Add default methods:

```rust
fn pre_tool_use_payload(&self, invocation: &ToolInvocation) -> HookPayload;
fn post_tool_use_payload(&self, output: &ToolOutput) -> HookPayload;
```

- [ ] **Step 4: Call hooks from registry/runtime before and after dispatch**

Pre-hook blocked means return a `ToolOutput::Function { success: false }` with a policy-denied message.

- [ ] **Step 5: Run hook and tool tests**

Run:

```bash
cargo test -p deepomni-hooks
cargo test -p deepomni-tools
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-tools crates/deepomni-hooks crates/deepomni-runtime
git commit -m "feat(tools): integrate pre and post tool hooks"
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

