# ArBeHarness v1 — Overall Design

## 1. Purpose and Scope
ArBeHarness v1 is a **high-performance, model-agnostic Rust agent harness** with a lightweight TUI. The harness is the product core; the TUI is an independent visualization/control layer.

v1 goals:
- Build a robust, extensible agent runtime fully under project control (no external agent-loop frameworks).
- Support multiple model providers (OpenAI, Anthropic, xAI, Mistral, Ollama) via a shared abstraction.
- Support modern agent features: MCP, agent skills, hooks.
- Implement strong memory and context management with room for experimentation.
- Support tool execution with explicit user approval.
- Persist all state/config/session assets under `~/.arbe/`.
- Run cross-platform from day one (Linux/macOS/Windows).

Out of scope for v1:
- Highly graphical/multi-pane TUI complexity.
- Distributed multi-agent orchestration.
- Full long-term memory production stack (vector/SQL retrieval in production mode).

---

## 2. Architecture Principles

1. **Strict separation of concerns**
   - `core` (agent harness logic) must not depend on `tui`.
   - `ui` consumes core APIs/events only.

2. **Pluggable by default**
   - Memory strategies, provider adapters, skill registries, and tool executors are trait-based modules.

3. **Deterministic and testable loop**
   - Agent loop modeled as explicit state transitions.

4. **Safety gates on side effects**
   - Tool execution requires policy check + user approval gate.

5. **Config-first experimentation**
   - Most strategies are selectable via config, not code changes.

6. **Cross-platform file and process behavior**
   - Avoid OS-specific assumptions in paths/process handling.

---

## 3. Proposed Workspace Layout

```text
ArBeHarness/
  Cargo.toml                # workspace
  crates/
    arbe-core/              # domain + runtime traits + agent loop
    arbe-providers/         # provider adapters (openai/anthropic/xai/mistral/ollama)
    arbe-memory/            # context mgmt, memory stores, compaction/truncation
    arbe-tools/             # tool registry, policy/approval, execution adapters
    arbe-mcp/               # MCP client integration and mapping
    arbe-skills/            # skills loading, validation, resolution
    arbe-hooks/             # lifecycle hook system
    arbe-storage/           # persistence API + fs implementation (~/.arbe)
    arbe-runtime/           # orchestration glue and session lifecycle
    arbe-tui/               # terminal UI
  docs/
  examples/
```

Notes:
- `arbe-runtime` composes modules; `arbe-core` remains lean and reusable.
- `arbe-tui` depends on `arbe-runtime` API/events, never vice versa.

---

## 4. Core Runtime Components

### 4.1 Agent Loop Engine
State machine (simplified):
1. `ReceiveUserInput`
2. `AssembleContext`
3. `PlanOrDirectRespond`
4. `ModelInference`
5. `InterpretOutput` (message/tool-call/structured command)
6. `ToolApproval` (if needed)
7. `ToolExecution`
8. `PostToolReflection` (optional)
9. `PersistTurn`
10. `EmitEvents`
11. `Idle`

Loop requirements:
- Cancellable and resumable.
- Bounded retries with categorized backoff.
- Every transition emits structured events.

### 4.2 Model Provider Abstraction
Provider trait responsibilities:
- Capability metadata (streaming, tool-calling format, max context, JSON mode).
- Unified request/response model.
- Streaming token/event support.
- Normalized error mapping (rate-limit, auth, timeout, bad request).

Per-provider modules:
- openai, anthropic, xai, mistral, ollama.

### 4.3 Skills System
- Skills represented as structured markdown manifests (+ optional schemas).
- Resolution order:
  1. Session-local skills
  2. Project/repo skills
  3. Global `~/.arbe/skills`
- Selection policy:
  - Explicit user-selected skills first.
  - Optional auto-selection by keyword/capability tags.

### 4.4 MCP Integration
- MCP server config in `~/.arbe/mcp/servers.toml`.
- Runtime discovers servers, performs handshake/capability negotiation.
- MCP tools exposed through unified tool registry.

### 4.5 Hooks System
Hook phases:
- `before_context_assembly`
- `before_model_call`
- `after_model_call`
- `before_tool_execute`
- `after_tool_execute`
- `on_error`
- `on_turn_complete`

Hooks can:
- Observe and transform payloads (when allowed by policy).
- Add diagnostics/metrics.

Guardrails:
- Hook timeout and failure isolation.
- Optional read-only hooks.

### 4.6 Tooling + Approval Gate
- Tool calls normalized into a typed invocation object.
- Approval policies:
  - `always_prompt`
  - `allowlisted_auto`
  - `denylisted_block`
  - `never_execute` (dry-run mode)
- User approval UX-friendly payload includes:
  - tool name
  - arguments
  - risk level
  - rationale/source

---

## 5. Memory & Context Management Strategy

### 5.1 Memory Layers
1. **Working memory (in-turn)**
2. **Session memory (short-term)** persisted per session
3. **Global notes memory** in `memory.md` style files
4. **Future long-term providers** (SQLite/vector) behind traits

### 5.2 Context Assembly Pipeline
1. Base system instructions
2. Global instructions (`~/.arbe/instructions/`)
3. Active skills
4. Session history selection
5. Notes/memory snippets
6. Tool results (recent + pinned)
7. User input

### 5.3 Context Budget Strategies (configurable)
- Sliding window
- Priority window (pin important turns)
- Semantic compaction (future/experimental)
- Hierarchical summarization (session checkpoints)

v1 must ship:
- Truncation strategy
- Compact-with-summary strategy
- Pinned messages support

### 5.4 Memory Files
Suggested layout:
```text
~/.arbe/
  sessions/
  config/
  skills/
  instructions/
  memory/
    global/memory.md
    projects/<project-id>/memory.md
```

---

## 6. Persistence Model (`~/.arbe/`)

Top-level proposal:
```text
~/.arbe/
  config/
    config.toml
    profiles/
  sessions/
    <session-id>/
      meta.json
      turns.jsonl
      artifacts/
  providers/
    credentials.toml           # or references to env/keychain
  skills/
    *.md
  instructions/
    *.md
  memory/
    global/memory.md
    projects/<project>/memory.md
  mcp/
    servers.toml
  logs/
    runtime.log
```

Design notes:
- JSONL for append-only turn/event logging.
- Atomic writes for config and metadata (temp file + rename).
- Optional encryption abstraction for sensitive values (future enhancement).

---

## 7. Configuration Philosophy

Config precedence:
1. CLI flags
2. Session overrides
3. Profile config
4. Global config
5. Built-in defaults

Config domains:
- provider selection + params
- model settings (temperature/top_p/max_tokens)
- context strategy + budget
- tool approval policy
- active hooks + skills
- logging/telemetry verbosity

---

## 8. Quality, Reliability, and Performance

### 8.1 Quality Standards
- Strong type modeling for domain events and commands.
- `clippy` clean, `rustfmt` enforced.
- High unit coverage in core components.
- Integration tests for provider adapters, context pipeline, tool approval.

### 8.2 Reliability
- Graceful degradation when provider/MCP fails.
- Clear categorized errors with user-facing action hints.
- Session recovery from persisted state.

### 8.3 Performance
- Streaming-first inference handling.
- Minimize allocations in hot loop paths.
- Async boundaries only where beneficial.

---

## 9. Cross-Platform Requirements
- Use `std::path`/`camino` abstractions for path handling.
- Avoid shell-specific assumptions in tool execution.
- Validate behavior on Linux/macOS/Windows in CI.

---

## 10. v1 Milestones

1. **Foundation**
   - workspace crates and domain models
   - event bus + state machine skeleton

2. **Provider + Loop MVP**
   - OpenAI + Ollama adapters first
   - end-to-end chat turn with streaming

3. **Memory + Context v1**
   - truncation + compact summary strategies
   - `memory.md` integration

4. **Tools + Approval**
   - local tool runner + approval gate
   - policy config

5. **Skills + Hooks + MCP baseline**
   - basic loading and lifecycle wiring

6. **TUI MVP**
   - simple chat UX + tool approval prompts

7. **Stabilization**
   - tests, docs, profiling, cross-platform QA
