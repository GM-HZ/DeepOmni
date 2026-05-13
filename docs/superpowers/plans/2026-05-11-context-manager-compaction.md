# Context Manager and Compaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn DeepOmni's context layer into a production-grade, token-aware memory system with normalization, rollback, model-driven compaction, and compaction events.

**Architecture:** `deepomni-context` owns history invariants and token accounting. `deepomni-engine` triggers compaction tasks through the session loop. `deepomni-agent` consumes only normalized prompt history and records compaction events through the journal.

**Tech Stack:** Rust 2024, existing `ModelMessage`, `ContextManager`, `JournalEntry`, Tokio, mock model providers for compaction tests.

---

## Source Documents

- `docs/03.md`
- `docs/01.md` for turn loop boundaries
- Existing implementation in `crates/deepomni-context/src/lib.rs`

## File Structure

- Create `crates/deepomni-context/src/manager.rs`: move `ContextManager` and token tracking.
- Create `crates/deepomni-context/src/normalize.rs`: call/output pairing and prompt cleanup.
- Create `crates/deepomni-context/src/compaction.rs`: compaction prompt helpers and compacted history construction.
- Modify `crates/deepomni-context/src/lib.rs`: module exports.
- Modify `crates/deepomni-engine/src/session_loop.rs`: trigger manual/auto compaction.
- Modify `crates/deepomni-agent/src/lib.rs`: consume `ContextManager::for_prompt()` and record usage.
- Modify `crates/deepomni-journal/src/lib.rs`: add richer compaction entries if needed.

## Task 1: Split ContextManager Into Modules

**Files:**
- Create: `crates/deepomni-context/src/manager.rs`
- Modify: `crates/deepomni-context/src/lib.rs`

- [ ] **Step 1: Run current context tests**

Run: `cargo test -p deepomni-context`

Expected: PASS before refactor.

- [ ] **Step 2: Move manager types**

Move:

- `ContextManager`
- `ContextSnapshot`
- `TokenUsageInfo`
- `TruncationPolicy`

into `manager.rs` and re-export them from `lib.rs`.

- [ ] **Step 3: Run context tests**

Run: `cargo test -p deepomni-context`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/deepomni-context/src
git commit -m "refactor(context): split context manager module"
```

## Task 2: Prompt Normalization

**Files:**
- Create: `crates/deepomni-context/src/normalize.rs`
- Modify: `crates/deepomni-context/src/manager.rs`

- [ ] **Step 1: Write failing normalization tests**

Add tests for:

- tool result without matching assistant tool call is dropped.
- assistant tool call without matching result gets a synthetic error result.
- image stripping is a no-op until multimodal support is represented.

Example:

```rust
#[test]
fn for_prompt_removes_orphan_tool_outputs() {
    let mut manager = ContextManager::new();
    manager.record_items(&[tool_output("call-missing", "orphan")], TruncationPolicy::None);
    assert!(manager.for_prompt().is_empty());
}
```

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p deepomni-context normalize`

Expected: FAIL.

- [ ] **Step 3: Implement normalization**

Implement:

```rust
pub fn normalize_history(items: &mut Vec<ModelMessage>) {
    ensure_call_outputs_present(items);
    remove_orphan_outputs(items);
}
```

Call it inside `ContextManager::for_prompt()`.

- [ ] **Step 4: Run context tests**

Run: `cargo test -p deepomni-context`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-context/src
git commit -m "feat(context): normalize prompt history"
```

## Task 3: Incremental Token Accounting

**Files:**
- Modify: `crates/deepomni-context/src/manager.rs`

- [ ] **Step 1: Write failing token tests**

Test `needs_compaction()` accounts for local items added after the last provider usage.

```rust
#[test]
fn token_usage_includes_local_items_after_last_usage() {
    let mut manager = ContextManager::new();
    manager.update_token_info(&Usage { prompt_tokens: 700, completion_tokens: 50, ..Default::default() }, Some(1000));
    manager.record_items(&[message("x".repeat(400))], TruncationPolicy::None);
    assert!(manager.needs_compaction(0.8));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-context token_usage_includes_local_items_after_last_usage`

Expected: FAIL.

- [ ] **Step 3: Track usage baseline**

Add:

```rust
last_usage_item_count: usize
```

When `update_token_info()` runs, set it to `items.len()`. Estimate tokens for items after that index and add them to `TokenUsageInfo.total_tokens`.

- [ ] **Step 4: Run context tests**

Run: `cargo test -p deepomni-context`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-context/src/manager.rs
git commit -m "feat(context): include local items in token accounting"
```

## Task 4: Rollback by User Turn

**Files:**
- Modify: `crates/deepomni-context/src/manager.rs`

- [ ] **Step 1: Write failing rollback tests**

Test `drop_last_n_user_turns(1)` removes the last user message and all following assistant/tool messages.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-context rollback`

Expected: FAIL.

- [ ] **Step 3: Implement rollback**

Add:

```rust
pub fn drop_last_n_user_turns(&mut self, n: usize) -> usize
```

Return number of removed items and increment `history_version` when items are removed.

- [ ] **Step 4: Normalize after rollback**

Call `normalize_history()` after removal.

- [ ] **Step 5: Run tests**

Run: `cargo test -p deepomni-context`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-context/src/manager.rs
git commit -m "feat(context): support rollback by user turn"
```

## Task 5: Model-Driven Compaction Helpers

**Files:**
- Create: `crates/deepomni-context/src/compaction.rs`
- Modify: `crates/deepomni-context/src/lib.rs`

- [ ] **Step 1: Write failing compaction helper tests**

Test:

- compaction prompt includes original history.
- compacted history preserves latest user message.
- compacted history starts with summary marker.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-context compaction`

Expected: FAIL.

- [ ] **Step 3: Implement helper functions**

Implement:

```rust
pub fn build_compaction_request(history: &[ModelMessage]) -> Vec<ModelMessage>;
pub fn build_compacted_history(summary: String, recent_user_messages: Vec<ModelMessage>) -> Vec<ModelMessage>;
```

Use a clear summary prefix:

```text
This conversation was compacted. Summary of previous context:
```

- [ ] **Step 4: Run context tests**

Run: `cargo test -p deepomni-context`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-context/src/compaction.rs crates/deepomni-context/src/lib.rs
git commit -m "feat(context): add compaction history helpers"
```

## Task 6: Engine Compaction Op

**Files:**
- Modify: `crates/deepomni-engine/src/session_loop.rs`
- Modify: `crates/deepomni-journal/src/lib.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing engine test**

Test `Op::Compact` writes `ContextCompactionStarted` and `ContextCompactionCompleted` journal entries and replaces the session `ContextManager` history.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-engine compact`

Expected: FAIL.

- [ ] **Step 3: Implement compact handler**

Use mock provider in tests. Production path should:

1. snapshot history
2. build compaction request
3. stream summary from provider
4. replace history
5. write journal entries
6. emit warning event through journal projection

- [ ] **Step 4: Run engine/runtime tests**

Run:

```bash
cargo test -p deepomni-engine
cargo test -p deepomni-runtime
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-engine crates/deepomni-journal crates/deepomni-runtime
git commit -m "feat(engine): handle manual context compaction"
```

## Task 7: Auto-Compaction Trigger

**Files:**
- Modify: `crates/deepomni-agent/src/lib.rs`
- Modify: `crates/deepomni-engine/src/session_loop.rs`

- [ ] **Step 1: Write failing trigger test**

Test a turn with usage over threshold queues compaction after the turn completes.

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p deepomni-runtime auto_compaction_queues_after_threshold`

Expected: FAIL.

- [ ] **Step 3: Record provider usage into ContextManager**

When provider returns usage, call `ContextManager::update_token_info()`.

- [ ] **Step 4: Queue compact op when threshold exceeded**

Use a default threshold of `0.8`, configurable later.

- [ ] **Step 5: Add failure breaker**

Track consecutive compaction failures and stop auto-triggering after 3 failures.

- [ ] **Step 6: Run tests**

Run: `cargo test --workspace`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/deepomni-agent crates/deepomni-engine crates/deepomni-runtime
git commit -m "feat(context): trigger auto compaction from token usage"
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

