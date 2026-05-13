# DeepOmni Runtime Wiring 与端到端验收计划

## Summary

目标不是继续补类型，而是把已经实现的 4 个 plan 从“组件可编译”推进到“系统按新架构真实运行”。完成标准必须由端到端行为证明：Server 接收 Op，Runtime 通过 SessionLoop 调度，Agent 使用新 Tool pipeline，Context/Trace 进入 hot-path，SSE/approval/replay 可验证，`test + clippy` 全绿。

推荐拆成 **6 个阶段**，每阶段都有独立验收指标，可以让 CC 长时间执行并按阶段汇报。

## Architecture Boundaries

- `deepomni-protocol`：只放稳定 wire/domain message 类型，例如 `Op`、`Submission`、`ServerMessage`、typed `TurnSettings`。不得依赖 Runtime、Server、State。
- `deepomni-engine`：负责 session loop、op dispatch、agent control、mailbox、compaction policy、approval coordination。不得包含 HTTP router 逻辑。
- `deepomni-runtime`：SDK facade 和 wiring 层，持有 `SessionServices`、`ActiveThread`、`SessionLoopHandle`、ContextManager、ApprovalStore、TraceWriter。
- `deepomni-server`：只负责 HTTP/SSE 协议适配，把 request 转成 `Op`，把 runtime/server notifications 转成响应。不得伪造 thread/approval/tool identity。
- `deepomni-agent`：执行单 turn，但工具调用必须通过 `ToolRouter + ToolCallRuntime`，不得绕过新 pipeline。
- `deepomni-tools`：负责工具规范化、路由、approval cache、parallel/exclusive execution。
- `deepomni-context`：负责 prompt history normalization、token tracking、compaction readiness。
- `deepomni-trace`：提供低开销 trace writer，并由 Runtime/Engine 在关键路径调用。

## Phase 0: Gate Clean 与 Adapter 安全修复

**Goal:** 当前代码达到基础交付门槛，且 Server protocol adapter 不再生成错误身份字段。

**Key Changes:**
- 修复 `cargo clippy --workspace --all-targets -- -D warnings` 的全部问题。
- 给 `SubmissionId::new()` 补 `Default` 或调整 API。
- 清理 unused imports、dead fields，若字段是未来 wiring 必需则通过真实使用保留。
- 重构 `ProtocolAdapter`：转换函数必须接收必要上下文，例如 `thread_id`、`turn_id`、`approval_id`、`tool_name`，或改为从 enriched runtime notification 转换。
- 禁止出现 `ThreadId::from_string("")`、空 `approval_id`、`"unknown"` tool name 作为正常协议输出。

**Acceptance Criteria:**
- `cargo test --workspace` 通过。
- `cargo clippy --workspace --all-targets -- -D warnings` 通过。
- 新增 adapter 测试：`ToolApprovalRequired` 输出包含真实 `thread_id`、`approval_id`、`tool_name`。
- 新增 adapter 测试：assistant delta、turn complete、turn failed 均携带正确 `thread_id`。

## Phase 1: SessionLoop 接入 Runtime

**Goal:** 每个 active thread 都有自己的 SessionLoop，Runtime submit/approve/reject 不再直接绕过 loop。

**Key Changes:**
- `ActiveThread` 保存 `SessionLoopHandle`。
- `Runtime::create_thread` 创建并注册 loop。
- `Runtime::submit_turn` 转成 `Op::UserInput` 或等价 submission，发送到 SessionLoop。
- `Runtime::approve_tool` / `reject_tool` 转成 approval Op，进入同一 thread loop。
- `SessionLoop` 不再只是 echo submission，而是调用 engine dispatcher，驱动 turn FSM。
- 保证同一 thread 内 submission 顺序执行，不同 thread 可并发。

**Acceptance Criteria:**
- 单元测试：同一 thread 连续提交 3 个 Op，执行顺序与提交顺序一致。
- 单元测试：两个 thread 的 Op 可并发，不共享 active turn 状态。
- 集成测试：`create_thread -> submit_turn` 通过 SessionLoop 完成，而不是直接调用旧路径。
- 集成测试：`NeedsApproval -> approve -> continue turn -> completed` 全流程走 SessionLoop。
- 旧 public Runtime API 保持兼容。

## Phase 2: Server Router 改为 Op/SessionLoop 路径

**Goal:** HTTP/SSE Server 使用新协议路径，不再直接调用同步 submit hot-path。

**Key Changes:**
- `POST /threads/:id/turns` 或当前等价 endpoint 构造 `Submission { id, op, created_at }`。
- Server 调用 Runtime 的 Op submit API，返回 `submission_id` 或当前协议兼容响应。
- approval/reject endpoint 构造 approval Op。
- SSE 从 runtime/server notification 或 journal projection 输出，不从伪造 adapter 输出。
- Replay 使用真实 journal/event projection，不丢 thread/turn/tool identity。

**Acceptance Criteria:**
- E2E test：HTTP 创建 thread，提交普通 turn，SSE 收到 `turn_started -> assistant_delta -> turn_completed`。
- E2E test：HTTP 提交需要 approval 的 tool turn，SSE 收到 approval request，HTTP approve 后 SSE 收到 tool result 和 turn completed。
- E2E test：SSE reconnect with `since_seq` 能 replay 之前事件，顺序稳定、seq 单调。
- 所有 Server 输出中无空 `thread_id`、空 `approval_id`、`unknown` tool name。

## Phase 3: ToolRouter + ToolCallRuntime 进入 Agent Hot Path

**Goal:** 真实 agent turn 使用新工具路由和并行执行，而不是测试里单独调用。

**Key Changes:**
- Agent 收到 model tool calls 后，统一交给 `ToolRouter` 解析、规范化、校验。
- `ToolRouter` 负责 model-visible name 到 registry handler 的映射。
- `ToolCallRuntime::execute_batch()` 负责同一轮 tool calls 的执行。
- non-mutating 工具使用 read lock 并行执行。
- mutating/exclusive 工具使用 write lock 串行执行。
- approval cache 在 tool evaluation 前生效。

**Acceptance Criteria:**
- 集成测试：一个 model response 返回两个 non-mutating tool calls，实际并发执行，总耗时小于串行阈值。
- 集成测试：两个 mutating tool calls 串行执行，顺序稳定。
- 集成测试：parallel + exclusive 混合时，exclusive 不与 parallel 写冲突。
- 集成测试：同一 session 内相同 approval key approve 一次后再次自动通过。
- Agent 代码中不再直接绕过 `ToolRouter + ToolCallRuntime` 执行 registry handler。

## Phase 4: ContextManager、Compaction、Trace 接入 Hot Path

**Goal:** Context 和 Trace 不只是模块存在，而是每个 turn 都真实使用。

**Key Changes:**
- Runtime 每个 active thread 持有一个 `ContextManager`。
- 新 turn、tool continuation、approval resume 均从 `ContextManager::for_prompt()` 构造 prompt history。
- turn 完成后通过 `record_items()` 写回 ContextManager。
- `for_prompt()` 至少完成基础 normalization：顺序稳定、非法 tool result 过滤或报错、重复/空消息处理策略明确。
- compaction threshold 由 usage/token estimate 驱动，进入 ready/pending 状态，但第一版可不自动调用模型压缩。
- Runtime/Engine 在 submit、turn start、model response、tool call、approval、turn complete/fail 写 trace。
- 默认使用 noop trace，测试可注入 memory trace。

**Acceptance Criteria:**
- 单元测试：ContextManager record 后 for_prompt 顺序稳定。
- 单元测试：非法 tool result 不会生成无效 provider request。
- 集成测试：multi-turn conversation 第二轮能看到第一轮 assistant output。
- 集成测试：tool continuation prompt 包含 assistant tool call 与 tool result，顺序正确。
- 集成测试：approval resume prompt 可从 ContextManager/journal 恢复。
- Trace 测试：一个完整 turn 至少记录 submit、turn_started、model_response、turn_completed。
- Trace hot path 不影响默认行为；noop trace 下无额外输出。

## Phase 5: Multi-Agent/Mailbox 最小闭环

**Goal:** 已实现的 AgentControl/Mailbox 不停留在结构层，至少支持一个可验证的 parent-child agent 通信闭环。

**Key Changes:**
- Runtime/Engine 注册 AgentControl。
- spawn subagent 创建 child session 或 child execution context。
- parent 与 child 通过 Mailbox 交换最小消息：spawn request、child result、child failed/cancelled。
- 定义 mailbox delivery phase：queued、delivered、completed。
- 第一版只要求本进程内通信，不要求跨进程或持久化恢复。

**Acceptance Criteria:**
- 单元测试：AgentRegistry 注册、查询、取消 agent。
- 集成测试：parent turn spawn child，child 返回 result，parent 收到 result 并继续。
- 集成测试：child failure 能转换成 parent 可见 error，不阻塞 session loop。
- 集成测试：取消 parent 时 child 被取消或标记 orphan policy，行为明确。

## Phase 6: Final End-to-End Release Gate

**Goal:** 用一组明确指标判断“4 个 plan 已完成”，不是靠主观 review。

**Required Commands:**
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

**Required E2E Scenarios:**
- Plain chat turn：Server -> Runtime -> SessionLoop -> Agent -> SSE completed。
- Tool turn：model emits tool call -> ToolRouter -> ToolCallRuntime -> tool result -> continuation -> completed。
- Approval turn：NeedsApproval -> pending approval visible -> approve endpoint -> resume -> completed。
- SSE replay：disconnect/reconnect 后按 seq replay，无重复、无错序。
- Multi-turn context：第二轮 prompt 包含第一轮历史，ContextManager version/token info 更新。
- Parallel tools：两个 safe tools 并发执行，一个 exclusive tool 不并发。
- Trace：memory trace 能看到完整 turn lifecycle。
- Multi-agent：parent spawn child，child result 回到 parent。

**Completion Metrics:**
- `cargo test --workspace` 至少包含并通过新增 E2E 场景；当前 214 tests 只是基线，完成后应增加对应测试数量。
- `clippy -D warnings` 0 warnings。
- Server 协议输出不得包含 placeholder identity。
- Runtime hot-path 中 `SessionLoop`、`ContextManager`、`ToolRouter`、`ToolCallRuntime`、`TraceWriter` 都有真实调用点和测试覆盖。
- 旧 API 兼容：现有 SDK/runtime tests 不需要大规模重写，只在内部路径切换到新架构。

## Implementation Rules For CC

- 每个 Phase 单独提交或至少单独 checkpoint。
- 每个 Phase 先写失败测试，再实现。
- 不允许用 placeholder 字段让测试通过。
- 不允许只加单元测试证明模块存在，必须至少有一个跨 Runtime/Server/Agent 的集成测试。
- 如果某 Phase 发现旧设计阻塞，先更新计划中的模块边界说明，再继续实现。
- 完成每个 Phase 后输出：修改文件、通过的测试命令、仍未完成的 acceptance criteria。

## Assumptions

- 现有 4 个 plan 的核心类型可以保留，不推倒重写。
- Runtime 仍保持对外兼容，Server/API 的外部行为除新增 `submission_id` 等必要字段外不做破坏性变更。
- 第一版 compaction 只要求进入 hot-path 和状态判定，不要求自动模型总结。
- 第一版 multi-agent 只要求本进程最小闭环，不要求分布式或持久化 mailbox。
