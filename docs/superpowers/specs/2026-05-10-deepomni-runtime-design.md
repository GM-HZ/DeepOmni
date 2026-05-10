# DeepOmni Runtime PRD and Technical Design

Date: 2026-05-10
Status: Draft for review
Owner: DeepOmni

## 1. Product Intent

DeepOmni is a DeepSeek-native agent runtime for production coding agents. The first version is not a CLI-first product. It is a Rust core runtime that can be embedded into a CLI, TUI, desktop server, remote server, worker process, mobile-controlled session, or future IDE client.

The product should feel like Codex in execution style: local-first, workspace-aware, event-driven, approval-gated, sandboxed, and suitable for SDK/server embedding. It should approach Claude Code in capability surface: rich tools, skills, plugins, commands, hooks, MCP, sub-agents, and eventually computer use. It should reuse DeepSeek-TUI's DeepSeek-native runtime ideas: OpenAI-compatible DeepSeek streaming, long-context workflow, HTTP/SSE runtime API, durable tasks, TUI separation, sandboxing, and stateful sessions.

DeepOmni's durable asset is the runtime protocol and Rust crate graph. UI clients are hosts over the same runtime.

## 2. Goals

- Provide a production-grade Rust agent runtime core.
- Expose a stable SDK and service contract for local and remote hosts.
- Support DeepSeek as a first-class model provider while preserving a provider registry for OpenAI-compatible, Ollama, vLLM, SGLang, and enterprise gateways.
- Support thread, turn, event, tool, approval, sandbox, plugin, skill, and MCP primitives from the first version.
- Keep the runtime independent from CLI/TUI/Desktop UI.
- Maximize reuse of the reference projects under `ref/` through design reuse, targeted source adaptation, and compatibility-inspired APIs.
- Establish a crate-level architecture that can scale to production testing, remote execution, computer use, and enterprise deployments.

## 3. Non-Goals for V1

- Full marketplace distribution.
- Full terminal TUI parity with Codex or Claude Code.
- Full computer use implementation.
- Full IDE integration.
- Full mobile app.
- Multi-tenant hosted SaaS control plane.
- Complex remote worker fleet scheduling.

V1 must still reserve stable boundaries for these later capabilities.

## 4. Reference Reuse Strategy

DeepOmni should not copy an entire upstream project wholesale. It should reuse designs and selectively adapt code where licensing and engineering fit are clear.

### 4.1 Codex Reference

Source: `ref/codex`

Reuse targets:

- Thread/session/turn concepts from `codex-rs/core/src/thread_manager.rs` and `codex-rs/core/src/codex_thread.rs`.
- Tool trait, registry, routing, diff streaming, and mutating-tool classification from `codex-rs/core/src/tools/registry.rs`.
- Approval, sandbox selection, retry, hooks, network policy, and tool orchestration ideas from `codex-rs/core/src/tools/orchestrator.rs`.
- Plugin manifest shape from `codex-rs/core-plugins/src/manifest.rs`.
- Plugin identity and capability summaries from `codex-rs/plugin/src/lib.rs`.
- Skills manager and injection concepts from `codex-rs/core-skills` and `codex-rs/core/src/skills.rs`.
- MCP client/server architecture from `codex-rs/rmcp-client`, `codex-rs/codex-mcp`, and `codex-rs/mcp-server`.
- State and rollout concepts from `codex-rs/core/src/state` and related thread store crates.

Adaptation rule:

- Preserve architectural concepts, event-driven shape, and safety boundaries.
- Rename and simplify protocol types for DeepOmni.
- Avoid coupling to OpenAI Responses-only assumptions.
- Keep DeepSeek Chat Completions streaming as a first-class path.

### 4.2 Claude Code Sourcemap Reference

Source: `ref/claude-code-sourcemap`

Reuse targets:

- Capability taxonomy: tools, commands, services, plugins, skills, remote sessions.
- UX and lifecycle concepts: slash commands, project commands, hooks, memory, MCP, agents, remote session manager.
- Plugin/skill/product affordances, not source-level copying.

Adaptation rule:

- Treat as a behavioral and product reference only.
- Do not rely on internal or reconstructed source structure as a production dependency.

### 4.3 DeepSeek-TUI Reference

Source: `ref/DeepSeek-TUI`

Reuse targets:

- DeepSeek provider defaults and OpenAI-compatible Chat Completions streaming.
- Runtime API concepts from `docs/RUNTIME_API.md`.
- High-level runtime architecture from `docs/ARCHITECTURE.md`.
- Durable thread/turn/event API.
- HTTP/SSE server shape.
- Task manager and background queue concepts.
- LSP post-edit diagnostics concept.
- Sandbox, hooks, skills, tools, plugins, and MCP boundary.

Adaptation rule:

- Prefer DeepSeek-TUI semantics for DeepSeek provider behavior.
- Rebuild the crate graph around a cleaner production SDK boundary.
- Use DeepSeek-TUI as the strongest reference for local server API and DeepSeek-native features.

## 5. Product Requirements

### 5.1 Runtime Core

The runtime must manage:

- Agent lifecycle.
- Thread lifecycle.
- Turn execution.
- Context assembly.
- Model streaming.
- Tool call loop.
- Approval requests.
- Sandbox selection.
- Event emission.
- Persistent state.
- Interruption and resume.

The runtime must not depend on a specific UI host.

### 5.2 SDK Embedding

DeepOmni must expose:

- Rust API for in-process embedding.
- HTTP/SSE API for local and remote hosts.
- TypeScript SDK for app/server/client integration.

Primary embedding modes:

- CLI host.
- Desktop app server.
- Remote server.
- Remote worker.
- Mobile-controlled client session.
- Future IDE host.

### 5.3 Tool System

V1 built-in tools:

- `read_file`
- `write_file`
- `edit_file`
- `apply_patch`
- `grep`
- `list_files`
- `shell_exec`
- `git_status`
- `git_diff`
- `git_apply`
- `todo_update`

Tool requirements:

- Every tool has a typed schema.
- Every tool declares mutability, sandbox needs, network needs, and approval default.
- Every tool emits structured lifecycle events.
- Every tool output can be truncated, summarized, or spilled to disk.
- Mutating tools always pass through the policy/orchestrator layer.

### 5.4 Skills

Skills are prompt-time behavior modules. A skill can include:

- `SKILL.md`
- supporting scripts
- assets
- templates
- metadata

Skills can be loaded from:

- Global user directory.
- Workspace `.deepomni/skills`.
- Plugin skills directory.
- Future remote marketplace cache.

Skills are not tools. They guide how the agent uses tools.

### 5.5 Plugins

Plugins are installable capability packages. A plugin can contribute:

- Skills.
- Tools.
- MCP server definitions.
- App connectors.
- Hooks.
- Slash commands.
- Auth providers.
- Native sidecars.
- Interface metadata.
- Permission requirements.

V1 plugin manager must support:

- Local plugin discovery.
- Manifest parsing.
- Enable/disable state.
- Capability listing.
- Permission declaration.
- Plugin-scoped data directory.

V1 can postpone remote marketplace install/update.

### 5.6 MCP

V1 must support MCP clients for external tool servers:

- stdio transport first.
- HTTP/SSE transport later.
- Tool discovery.
- Tool schema conversion.
- Approval and policy integration.

MCP tools must be indistinguishable from native tools at the agent loop boundary.

### 5.7 Computer Use

Computer Use is a future high-privilege plugin, not a core runtime feature.

Core V1 must reserve:

- Permission types: `screen.capture`, `input.control`, `window.inspect`, `clipboard.read`, `clipboard.write`.
- Tool classes: screenshot, click, type, keypress, scroll, drag, observe.
- Runtime adapter boundary for local macOS, Windows, Linux, remote browser, and remote desktop.
- Approval and redaction hooks for sensitive UI operations.

### 5.8 State and Persistence

The runtime must persist:

- Threads.
- Turns.
- Events.
- Tool calls.
- Usage.
- Approval decisions.
- Plugin state.
- Skill/plugin capability cache.
- Optional workspace snapshots.

V1 can use SQLite for structured state and filesystem blobs for large artifacts.

### 5.9 Security

Security requirements:

- Localhost-only server by default.
- Optional bearer token for HTTP/SSE API.
- Mutating tool approval in normal agent mode.
- YOLO/trusted mode must be explicit.
- Sandbox policy must be represented in the runtime protocol.
- Plugin permissions must be declared and visible.
- MCP tools must not bypass the policy engine.
- Computer Use must be disabled by default.

## 6. Runtime Architecture

```text
Hosts
├── deepomni-cli
├── deepomni-tui
├── desktop app
├── remote server
├── worker
└── mobile/web client
        │
        ▼
deepomni-server / deepomni-sdk
        │
        ▼
deepomni-runtime
├── agent loop
├── thread manager
├── turn runner
├── context manager
├── tool orchestrator
├── policy engine
├── event bus
└── state service
        │
        ├── model providers
        ├── tools
        ├── skills
        ├── plugins
        ├── hooks
        ├── MCP
        └── sandbox
```

## 7. Crate Design

The workspace should be split into small crates with clear ownership. Crates should avoid circular dependencies. Protocol crates should not depend on runtime implementation crates.

### 7.1 Workspace Layout

```text
crates/
├── deepomni-protocol
├── deepomni-runtime
├── deepomni-agent
├── deepomni-model-provider
├── deepomni-provider-deepseek
├── deepomni-tools
├── deepomni-tool-builtin
├── deepomni-policy
├── deepomni-sandbox
├── deepomni-state
├── deepomni-events
├── deepomni-context
├── deepomni-skills
├── deepomni-plugin
├── deepomni-mcp
├── deepomni-hooks
├── deepomni-server
├── deepomni-task
├── deepomni-computer-use
├── deepomni-cli
└── deepomni-test-support
sdk/
└── typescript
```

### 7.2 `deepomni-protocol`

Purpose:

- Stable API and wire types.

Contains:

- `ThreadId`, `TurnId`, `EventSeq`, `ToolCallId`.
- Thread, turn, event models.
- Request/response DTOs.
- Tool schema types.
- Approval request/decision types.
- Permission profile types.
- Sandbox policy types.
- Usage and cost types.
- Error models.

Dependencies:

- `serde`
- `schemars`
- `thiserror`
- `time`
- `uuid`

Must not depend on runtime, server, tools, or provider crates.

Testing:

- JSON compatibility snapshot tests.
- Schema generation tests.
- Backward compatibility tests for event payloads.

### 7.3 `deepomni-runtime`

Purpose:

- Main host-agnostic runtime facade.

Contains:

- `Runtime`.
- `RuntimeBuilder`.
- `RuntimeConfig`.
- lifecycle start/shutdown.
- thread creation/resume/fork.
- turn submission.
- approval submission.
- interruption.
- event subscription.

Dependencies:

- `deepomni-protocol`
- `deepomni-agent`
- `deepomni-state`
- `deepomni-events`
- `deepomni-policy`
- `deepomni-tools`
- `deepomni-plugin`
- `deepomni-skills`
- `deepomni-mcp`
- `deepomni-model-provider`

Testing:

- Runtime lifecycle integration tests.
- Resume/interruption tests.
- Concurrent thread tests.
- Host-agnostic API tests.

### 7.4 `deepomni-agent`

Purpose:

- Agent loop and turn runner.

Contains:

- `AgentLoop`.
- `TurnRunner`.
- model streaming parser.
- tool call detection.
- tool result continuation.
- context compaction trigger.
- sub-agent control boundary.

References:

- Codex thread/turn concepts.
- DeepSeek-TUI turn loop.

Testing:

- Mock provider turn-loop tests.
- Tool-call loop tests.
- Streaming delta parser tests.
- Interrupted turn tests.
- Context compaction trigger tests.

### 7.5 `deepomni-model-provider`

Purpose:

- Provider abstraction and registry.

Contains:

- `ModelProvider` trait.
- `ModelStream`.
- `ModelRequest`.
- `ModelEvent`.
- provider registry.
- model metadata and capability discovery.

Testing:

- Mock provider.
- Provider selection tests.
- Model capability tests.

### 7.6 `deepomni-provider-deepseek`

Purpose:

- DeepSeek-native provider implementation.

Contains:

- OpenAI-compatible Chat Completions client.
- streaming parser.
- tool call normalization.
- DeepSeek-specific headers and base URL config.
- optional beta endpoint support.
- token/cost metadata mapping.

References:

- DeepSeek-TUI client and provider behavior.

Testing:

- Fixture-based streaming parser tests.
- Request serialization tests.
- Error mapping tests.
- Retry and timeout tests.
- No live API dependency in default test suite.

### 7.7 `deepomni-tools`

Purpose:

- Tool traits, registry, router, output model.

Contains:

- `ToolHandler` trait.
- `ToolRegistry`.
- `ToolInvocation`.
- `ToolOutput`.
- `ToolSpec`.
- mutability classification.
- tool argument diff streaming.
- MCP/native unified tool wrapper.

References:

- Codex `ToolHandler`, registry, router, and output concepts.

Testing:

- Registry tests.
- Duplicate-name tests.
- Schema tests.
- Mutability classification tests.
- Tool output truncation tests.

### 7.8 `deepomni-tool-builtin`

Purpose:

- Production built-in tools.

Contains:

- file tools.
- shell tool.
- grep/list tools.
- apply patch.
- git tools.
- todo tools.

Dependencies:

- `deepomni-tools`
- `deepomni-policy`
- `deepomni-sandbox`

Testing:

- Temp workspace tests for every tool.
- Golden output tests.
- Permission and sandbox behavior tests.
- Cross-platform path tests.
- Apply-patch failure tests.

### 7.9 `deepomni-policy`

Purpose:

- Approval, permission, network, and execution policy decisions.

Contains:

- `ApprovalPolicy`.
- `PermissionProfile`.
- `PolicyDecision`.
- `NetworkPolicy`.
- approval cache.
- trusted/agent/plan modes.

References:

- Codex approval policy and tool orchestration semantics.

Testing:

- Policy matrix tests.
- Mutating vs read-only decisions.
- Network approval tests.
- Trusted mode tests.
- Denied command tests.

### 7.10 `deepomni-sandbox`

Purpose:

- Platform sandbox abstraction.

Contains:

- `SandboxManager`.
- macOS Seatbelt adapter.
- Linux Landlock adapter.
- Windows placeholder adapter.
- no-sandbox adapter.
- sandbox denial classification.

References:

- Codex sandbox and DeepSeek-TUI sandbox architecture.

Testing:

- Unit tests for policy generation.
- Platform-gated integration tests.
- Denial classification tests.

### 7.11 `deepomni-state`

Purpose:

- Durable persistence.

Contains:

- SQLite store.
- thread store.
- turn store.
- event store.
- tool call store.
- plugin state store.
- migration system.
- blob/artifact store.

Testing:

- Migration tests from empty DB.
- Round-trip serialization tests.
- Event replay tests.
- Corruption/recovery tests.
- Concurrent read tests.

### 7.12 `deepomni-events`

Purpose:

- Runtime event bus and event replay.

Contains:

- monotonic event sequence.
- broadcast subscriptions.
- persisted event replay.
- SSE adapter helpers.
- event filters.

Testing:

- Ordering tests.
- Replay since sequence tests.
- Backpressure tests.
- Subscriber drop tests.

### 7.13 `deepomni-context`

Purpose:

- Context assembly and compaction.

Contains:

- system prompt fragments.
- project instructions.
- skills injection.
- plugin capability summaries.
- environment context.
- conversation history selection.
- compaction interface.

References:

- Codex context fragments and skill/plugin instruction renderers.

Testing:

- Prompt assembly golden tests.
- Skill injection tests.
- Plugin capability rendering tests.
- Token budget tests with mock tokenizer.

### 7.14 `deepomni-skills`

Purpose:

- Skill discovery, loading, rendering, and selection.

Contains:

- `Skill`.
- `SkillManager`.
- global/workspace/plugin skill roots.
- mention syntax.
- metadata parser.
- asset/script path resolution.

Testing:

- Directory discovery tests.
- Frontmatter parsing tests.
- Plugin skill namespace tests.
- Path traversal rejection tests.

### 7.15 `deepomni-plugin`

Purpose:

- Plugin manifest, install state, capability loading.

Contains:

- `PluginManifest`.
- `PluginManager`.
- local plugin discovery.
- enable/disable.
- capability summaries.
- permissions.
- plugin data root.

Manifest fields:

```json
{
  "id": "deepomni.example",
  "name": "Example",
  "version": "0.1.0",
  "description": "Example plugin",
  "skills": "./skills",
  "tools": "./tools.json",
  "mcpServers": "./mcp.json",
  "hooks": "./hooks.json",
  "commands": "./commands",
  "permissions": ["filesystem.read"],
  "runtime": {
    "type": "none"
  },
  "interface": {
    "displayName": "Example",
    "capabilities": ["example"]
  }
}
```

References:

- Codex plugin manifest and capability summary.

Testing:

- Manifest parsing tests.
- Relative path validation tests.
- Enable/disable state tests.
- Capability summary tests.
- Malformed manifest tests.

### 7.16 `deepomni-mcp`

Purpose:

- MCP client and native tool bridge.

Contains:

- stdio MCP client.
- server lifecycle.
- tool discovery.
- tool call bridge.
- MCP resource support later.

Testing:

- In-process fake MCP server tests.
- Discovery tests.
- Tool schema conversion tests.
- Server crash tests.

### 7.17 `deepomni-hooks`

Purpose:

- Lifecycle hooks.

Hook points:

- session start.
- user prompt submitted.
- before tool call.
- permission requested.
- after tool call.
- turn completed.
- session stop.

Testing:

- Hook matching tests.
- Hook timeout tests.
- Hook failure isolation tests.
- Context injection tests.

### 7.18 `deepomni-server`

Purpose:

- HTTP/SSE/WebSocket runtime API.

Contains:

- Axum server.
- authentication middleware.
- thread endpoints.
- turn endpoints.
- approval endpoints.
- event SSE endpoint.
- health and introspection endpoints.

Endpoints:

```text
GET  /health
POST /v1/threads
GET  /v1/threads
GET  /v1/threads/:id
PATCH /v1/threads/:id
POST /v1/threads/:id/resume
POST /v1/threads/:id/fork
POST /v1/threads/:id/turns
POST /v1/threads/:id/turns/:turn_id/approve
POST /v1/threads/:id/turns/:turn_id/reject
POST /v1/threads/:id/turns/:turn_id/interrupt
GET  /v1/threads/:id/events?since_seq=0
GET  /v1/tools
GET  /v1/skills
GET  /v1/plugins
PATCH /v1/plugins/:id
GET  /v1/mcp/servers
GET  /v1/mcp/tools
```

Testing:

- API integration tests.
- SSE replay tests.
- Auth tests.
- Cancellation tests.
- Concurrent clients tests.

### 7.19 `deepomni-task`

Purpose:

- Durable background task queue.

V1 status:

- Skeleton crate with protocol integration.
- Full implementation can land after runtime MVP.

Contains later:

- task queue.
- worker pool.
- task timelines.
- task artifacts.
- scheduled automation hook.

References:

- DeepSeek-TUI durable task manager.

### 7.20 `deepomni-computer-use`

Purpose:

- Future high-privilege plugin/tool adapter crate.

V1 status:

- Protocol and permission definitions only.
- No default runtime activation.

Contains later:

- screenshot.
- click.
- type.
- keypress.
- scroll.
- drag.
- observe.
- platform adapters.

Testing later:

- Mock desktop adapter tests.
- Screenshot redaction tests.
- Approval tests.
- Remote browser adapter tests.

### 7.21 `deepomni-cli`

Purpose:

- Minimal debug host for runtime.

V1 commands:

```text
deepomni run
deepomni serve
deepomni doctor
deepomni plugin list
deepomni skill list
```

The CLI must remain thin. It should not own runtime logic.

### 7.22 `deepomni-test-support`

Purpose:

- Shared test fixtures and fakes.

Contains:

- mock model provider.
- fake streaming fixtures.
- temp workspace helpers.
- fake MCP server.
- fake tool handlers.
- event assertions.
- snapshot helpers.

## 8. Agent Loop

The turn runner follows this sequence:

1. Receive user input.
2. Persist turn start.
3. Assemble context.
4. Send request to provider.
5. Stream assistant deltas.
6. Detect tool calls.
7. For each tool call, route through tool orchestrator.
8. Request approval if policy requires it.
9. Execute under selected sandbox.
10. Persist tool result and emit events.
11. Continue model turn with tool results.
12. Complete turn or compact context if needed.

All externally visible state changes emit events.

## 9. Runtime Event Protocol

Core events:

```text
thread.created
thread.updated
thread.archived
turn.started
turn.steered
turn.interrupted
assistant.message.delta
assistant.message.completed
tool.call.started
tool.call.arguments.delta
tool.call.requires_approval
tool.call.approved
tool.call.rejected
tool.call.completed
tool.call.failed
context.compaction.started
context.compaction.completed
turn.completed
turn.failed
runtime.warning
```

Events must be:

- Ordered per thread.
- Persisted before delivery where possible.
- Replayable by `since_seq`.
- JSON schema documented.
- Stable across hosts.

## 10. SDK API

### 10.1 Rust SDK

Example:

```rust
let runtime = RuntimeBuilder::new()
    .workspace("/repo")
    .provider(deepseek_provider)
    .build()
    .await?;

let thread = runtime.create_thread(CreateThreadRequest {
    workspace: "/repo".into(),
    model: Some("deepseek-chat".into()),
    ..Default::default()
}).await?;

let mut events = runtime.subscribe(thread.id).await?;
runtime.submit_turn(thread.id, "Fix the failing tests").await?;
```

### 10.2 TypeScript SDK

Example:

```ts
const client = new DeepOmniClient({ baseUrl: "http://127.0.0.1:7878" });

const thread = await client.threads.create({
  workspace: "/repo",
  model: "deepseek-chat",
});

for await (const event of client.threads.run(thread.id, {
  input: "Fix the failing tests",
})) {
  console.log(event);
}
```

## 11. Test Strategy

DeepOmni is production-targeted. Every crate must have meaningful tests before it is considered complete.

### 11.1 Coverage Targets

- Protocol crates: 90%+ line coverage.
- Policy, tools, state, provider parser: 85%+ line coverage.
- Runtime and server: integration coverage over all critical flows.
- CLI: smoke coverage.

Coverage percentage is not enough. Critical behavior must have scenario tests.

### 11.2 Test Classes

Unit tests:

- Protocol serialization.
- Tool schema validation.
- Policy matrix.
- Provider streaming parser.
- Manifest parsing.
- Context rendering.

Integration tests:

- Full turn with mock model and built-in tool.
- Approval required and approved.
- Approval rejected.
- Tool failure returned to model.
- Interrupt mid-turn.
- Resume thread from state.
- SSE replay.
- MCP fake server tool call.

Golden tests:

- Prompt/context rendering.
- Event JSON.
- Plugin manifest parsing.
- Tool outputs.

Property tests:

- Event sequence monotonicity.
- Path normalization.
- Manifest path validation.

Platform tests:

- macOS sandbox when available.
- Linux sandbox when available.
- Windows no-op or placeholder behavior.

Manual/live tests:

- DeepSeek live provider smoke test behind an ignored test flag.
- Computer use live tests later, disabled by default.

### 11.3 CI Gates

Minimum CI commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test --workspace --features integration
```

Optional later:

```bash
cargo llvm-cov --workspace --fail-under-lines 80
```

## 12. Production Readiness Requirements

Before a runtime milestone is called production-ready:

- All crates compile with warnings denied.
- Critical flows have integration tests.
- Runtime API has schema docs.
- Event protocol has compatibility tests.
- State migrations are tested.
- No mutating tool bypasses policy.
- Plugin permissions are visible before enablement.
- Server auth works for non-local exposure.
- Logs do not leak API keys.
- Tool output truncation prevents unbounded memory growth.
- Cancellation works for long-running turns/tools.

## 13. Milestones

### Milestone 0: Workspace and Protocol

- Create Rust workspace.
- Add protocol crate.
- Add event models.
- Add basic state schema.
- Add test-support crate.

Exit criteria:

- Protocol schema tests pass.
- Example event JSON snapshots pass.

### Milestone 1: Runtime MVP

- Runtime builder.
- Thread manager.
- Turn runner.
- Mock provider.
- Event bus.
- SQLite persistence.

Exit criteria:

- Mock full-turn integration test passes.
- Resume thread test passes.

### Milestone 2: DeepSeek Provider and Built-in Tools

- DeepSeek provider.
- File/shell/grep/apply_patch/git tools.
- Tool orchestrator.
- Approval policy.

Exit criteria:

- Tool-call loop with mock provider passes.
- DeepSeek live smoke test is available behind ignored flag.

### Milestone 3: Server and SDK

- HTTP server.
- SSE events.
- approval endpoints.
- TS SDK.
- minimal CLI.

Exit criteria:

- SDK can create thread, run turn, stream events, and approve tool call.

### Milestone 4: Skills, Plugins, MCP

- Skill manager.
- Plugin manager.
- Manifest loader.
- MCP stdio client.
- Hook runtime.

Exit criteria:

- Local plugin can add a skill and MCP tool.
- Plugin enable/disable updates runtime capabilities.

### Milestone 5: Remote and Computer Use Foundations

- Remote worker protocol draft.
- Durable task skeleton.
- Computer Use permission and tool protocol.
- Local adapter abstraction.

Exit criteria:

- Computer Use plugin can be discovered but remains disabled by default.
- Remote executor protocol has documented test fixtures.

## 14. Open Questions

- Should V1 use SQLite only, or SQLite plus append-only JSONL event logs?
- Should plugin manifests use JSON only, or support TOML as a first-class format?
- Should the runtime API align more closely with DeepSeek-TUI `/v1/threads` or Codex app-server protocol names?
- How much upstream Codex code can be source-adapted under license and maintenance constraints?
- Should remote server mode support multi-user auth in V1, or only single-token deployment?

## 15. Recommended Decisions

- Use Rust core for V1.
- Use SQLite as the primary state store, with optional event JSONL export later.
- Use JSON plugin manifests first for Codex compatibility.
- Use DeepSeek-TUI-style HTTP/SSE endpoint shape.
- Use Codex-style tool orchestration and plugin manifest concepts.
- Keep Computer Use as a plugin boundary from day one.
- Build test support first so runtime behavior can be developed with deterministic mock providers.

