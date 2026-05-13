# Op/Event Session Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert DeepOmni from a synchronous `submit_turn()` call chain into a Codex-style harness where clients submit `Op`s and consume projected `EventFrame`s asynchronously.

**Architecture:** `Runtime` stays as the SDK-facing facade, while `deepomni-engine` owns `Submission`, `Op`, and a per-thread `SessionLoop`. The existing journal remains the durable source of truth; `EventFrame` remains the host-safe projection.

**Tech Stack:** Rust 2024, Tokio `mpsc`/`broadcast`, existing `deepomni-engine`, `deepomni-runtime`, `deepomni-journal`, `deepomni-protocol`, SQLite state store.

---

## Source Documents

- `docs/01.md`
- `docs/04.md` sections on App Server protocol boundaries
- Existing plan: `.claude/plans/turn-journal-refactoring-plan.md`

## File Structure

- Create `crates/deepomni-protocol/src/op.rs`: public `Op`, `SubmissionId`, `Submission`, and structured user input types.
- Modify `crates/deepomni-protocol/src/lib.rs`: export `op`.
- Create `crates/deepomni-engine/src/session_loop.rs`: per-thread background loop consuming submissions.
- Modify `crates/deepomni-engine/src/lib.rs`: export session loop types.
- Modify `crates/deepomni-runtime/src/lib.rs`: create one session loop per active thread and expose `submit()`.
- Modify `crates/deepomni-server/src/lib.rs`: route HTTP turn/approval/cancel endpoints through `Runtime::submit()`.
- Add tests in `crates/deepomni-engine/src/session_loop.rs` and `crates/deepomni-runtime/src/lib.rs`.

## Task 1: Protocol Op Types

**Files:**
- Create: `crates/deepomni-protocol/src/op.rs`
- Modify: `crates/deepomni-protocol/src/lib.rs`

- [ ] **Step 1: Write failing protocol tests**

Add tests in `op.rs`:

```rust
#[test]
fn op_user_input_serializes_with_turn_settings() {
    let op = Op::UserInput {
        thread_id: ThreadId::from_string("thread-1"),
        input: vec![UserInput::Text { text: "hello".into() }],
        settings: TurnSettings {
            model: Some("deepseek-chat".into()),
            cwd: Some("/tmp/repo".into()),
            approval_policy: None,
            sandbox_policy: None,
        },
    };
    let json = serde_json::to_value(op).unwrap();
    assert_eq!(json["type"], "user_input");
    assert_eq!(json["settings"]["model"], "deepseek-chat");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-protocol op_user_input_serializes_with_turn_settings`

Expected: FAIL because `op` module does not exist.

- [ ] **Step 3: Implement protocol types**

Implement:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SubmissionId(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub id: SubmissionId,
    pub op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInput {
    Text { text: String },
    Image { image_url: String },
    LocalImage { path: PathBuf },
    Skill { name: String, path: PathBuf },
    Mention { name: String, path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnSettings {
    pub model: Option<String>,
    pub cwd: Option<PathBuf>,
    pub approval_policy: Option<ApprovalPolicy>,
    pub sandbox_policy: Option<SandboxPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    UserInput { thread_id: ThreadId, input: Vec<UserInput>, settings: TurnSettings },
    SteerInput { thread_id: ThreadId, input: Vec<UserInput> },
    ApprovalDecision { thread_id: ThreadId, approval_id: String, approved: bool },
    Cancel { thread_id: ThreadId },
    Compact { thread_id: ThreadId },
}
```

- [ ] **Step 4: Run protocol tests**

Run: `cargo test -p deepomni-protocol`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-protocol/src/lib.rs crates/deepomni-protocol/src/op.rs
git commit -m "feat(protocol): add op submission types"
```

## Task 2: Engine SessionLoop Skeleton

**Files:**
- Create: `crates/deepomni-engine/src/session_loop.rs`
- Modify: `crates/deepomni-engine/src/lib.rs`

- [ ] **Step 1: Write failing loop tests**

Test that `submit()` returns immediately and the loop receives submissions in order.

```rust
#[tokio::test]
async fn session_loop_accepts_ordered_submissions() {
    let (handle, mut observed) = SessionLoop::spawn_test();
    let first = handle.submit(Op::Cancel { thread_id: ThreadId::from_string("t") }).unwrap();
    let second = handle.submit(Op::Compact { thread_id: ThreadId::from_string("t") }).unwrap();
    assert_ne!(first, second);
    assert_eq!(observed.recv().await.unwrap().id, first);
    assert_eq!(observed.recv().await.unwrap().id, second);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-engine session_loop_accepts_ordered_submissions`

Expected: FAIL because `SessionLoop` is missing.

- [ ] **Step 3: Implement session loop handle**

Implement:

- `SessionLoopHandle { submission_tx }`
- `SessionLoop::spawn(services, state) -> SessionLoopHandle`
- `SessionLoop::run()` with a `while let Some(submission)` loop
- test-only constructor `spawn_test()`

- [ ] **Step 4: Run engine tests**

Run: `cargo test -p deepomni-engine`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-engine/src/lib.rs crates/deepomni-engine/src/session_loop.rs
git commit -m "feat(engine): add session loop submission queue"
```

## Task 3: Runtime Facade Submit API

**Files:**
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing runtime test**

Add a test showing `Runtime::submit()` returns a `SubmissionId` without waiting for a model response.

```rust
#[tokio::test]
async fn runtime_submit_returns_submission_id_immediately() {
    let runtime = RuntimeBuilder::new().workspace("/tmp/test").build().await.unwrap();
    let thread = runtime.create_thread(CreateThreadRequest {
        workspace: "/tmp/test".into(),
        ..Default::default()
    }).await.unwrap();

    let id = runtime.submit(Op::Cancel { thread_id: thread.id.clone() }).await.unwrap();
    assert!(!id.as_str().is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-runtime runtime_submit_returns_submission_id_immediately`

Expected: FAIL because `Runtime::submit` is missing.

- [ ] **Step 3: Add per-thread session handles**

Add to `ActiveThread`:

```rust
pub session_loop: Option<SessionLoopHandle>,
```

Create a loop when creating or resuming a thread.

- [ ] **Step 4: Implement `Runtime::submit()`**

Route `Op` to the thread's `SessionLoopHandle`.

- [ ] **Step 5: Run runtime tests**

Run: `cargo test -p deepomni-runtime`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-runtime/src/lib.rs
git commit -m "feat(runtime): expose asynchronous op submission"
```

## Task 4: Move Turn Start Into SessionLoop

**Files:**
- Modify: `crates/deepomni-engine/src/session_loop.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing integration test**

Test `Op::UserInput` causes `turn.started` and `turn.completed` journal entries through the loop.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-runtime op_user_input_runs_turn_through_session_loop`

Expected: FAIL because `UserInput` is not handled.

- [ ] **Step 3: Move existing `submit_turn` body behind loop handler**

Create internal method:

```rust
async fn handle_user_input(&mut self, submission: Submission) -> Result<(), RuntimeError>
```

Use existing runtime services and `TurnRunner`.

- [ ] **Step 4: Keep compatibility facade**

`Runtime::submit_turn()` should submit `Op::UserInput` and then wait for the terminal turn event only for legacy callers. Add a TODO to remove blocking behavior once server/SDK are migrated.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p deepomni-runtime submit_turn
cargo test -p deepomni-agent
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/deepomni-engine/src/session_loop.rs crates/deepomni-runtime/src/lib.rs
git commit -m "feat(runtime): run turns through session loop"
```

## Task 5: Approval, Cancel, and Steer Ops

**Files:**
- Modify: `crates/deepomni-engine/src/session_loop.rs`
- Modify: `crates/deepomni-runtime/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Add tests for:

- `Op::ApprovalDecision` resumes pending tool approval.
- `Op::Cancel` marks active turn interrupted or failed.
- `Op::SteerInput` records a journal entry while a turn is active.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p deepomni-runtime op_`

Expected: FAIL.

- [ ] **Step 3: Implement op handlers**

Map:

- `ApprovalDecision` → existing `approve_tool` / `reject_tool` internal logic.
- `Cancel` → write `TurnFailed` or future `TurnInterrupted` journal entry.
- `SteerInput` → write contextual user fragment journal entry and queue for next model step.

- [ ] **Step 4: Run runtime tests**

Run: `cargo test -p deepomni-runtime`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-engine/src/session_loop.rs crates/deepomni-runtime/src/lib.rs
git commit -m "feat(engine): handle approval cancel and steer ops"
```

## Task 6: Server Migration

**Files:**
- Modify: `crates/deepomni-server/src/lib.rs`

- [ ] **Step 1: Write failing server tests**

Test that `POST /v1/threads/:id/turns` submits an op and returns a submission/turn handle without depending on synchronous turn completion.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p deepomni-server`

Expected: FAIL for new behavior.

- [ ] **Step 3: Route server endpoints through `Runtime::submit()`**

Keep wire compatibility by returning existing `Turn` fields plus `submission_id` if protocol supports it. If not, add a backward-compatible optional field.

- [ ] **Step 4: Run server and runtime tests**

Run:

```bash
cargo test -p deepomni-server
cargo test -p deepomni-runtime
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/deepomni-server/src/lib.rs crates/deepomni-protocol/src/*.rs
git commit -m "feat(server): submit turns through op protocol"
```

## Final Verification

- [ ] Run formatting:

```bash
cargo fmt --all -- --check
```

- [ ] Run tests:

```bash
cargo test --workspace
```

- [ ] Run clippy:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] Review for blocking issues:

```bash
git diff --check
git diff --stat
```

