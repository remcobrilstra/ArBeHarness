# ArBeHarness v1 — Implementation Plan

## 1. Goal
Deliver a production-grade v1 of the ArBeHarness core + TUI with strong extensibility, safety, and cross-platform support.

## 2. Delivery Strategy
Use phased vertical slices, each ending in a usable checkpoint:
- Core compiles + tested
- One or more real user flows work end-to-end
- No phase leaves architecture debt hidden in UI

---

## 3. Phase Plan

## Phase 0 — Project Foundation (Week 1)
### Scope
- Set up Rust workspace and crate boundaries.
- Define domain types and event model.
- Add CI baseline.

### Deliverables
- Workspace with crates:
  - `arbe-core`
  - `arbe-runtime`
  - `arbe-storage`
  - `arbe-providers`
  - `arbe-memory`
  - `arbe-tools`
  - `arbe-skills`
  - `arbe-hooks`
  - `arbe-mcp`
  - `arbe-tui`
- CI:
  - format (`rustfmt`)
  - lint (`clippy -D warnings`)
  - tests (`cargo test`)
  - OS matrix: ubuntu-latest, macos-latest, windows-latest
- Initial `README` architecture overview.

### Exit Criteria
- `cargo check`, `cargo test`, `cargo clippy` passing across CI matrix.

---

## Phase 1 — Loop Skeleton + Persistence (Week 2)
### Scope
- Build harness state machine and persistence primitives.
- Implement session create/resume/append.

### Deliverables
- Agent loop states/events defined in `arbe-core`.
- `arbe-storage` filesystem backend rooted at `~/.arbe/`.
- JSONL turn/event append model.
- Session metadata lifecycle (`created`, `active`, `closed`, `failed`).

### Exit Criteria
- CLI or test harness can:
  - create session
  - append turn
  - resume session
  - recover after forced interruption

---

## Phase 2 — Provider Abstraction + First Adapters (Week 3)
### Scope
- Unified model provider interface.
- Add first hosted and local adapters.

### Deliverables
- Provider trait + capability metadata.
- OpenAI adapter (streaming + non-streaming).
- Ollama adapter (streaming + non-streaming).
- Error normalization layer.

### Exit Criteria
- Same prompt runs on both adapters by config switch only.
- Streaming events emitted uniformly.

---

## Phase 3 — Context & Memory v1 (Week 4)
### Scope
- Build context assembly pipeline.
- Ship initial strategy plugins.

### Deliverables
- Context builder pipeline:
  - system instructions
  - profile/global instructions
  - skills
  - selected session history
  - memory.md fragments
  - user turn
- Strategies:
  - truncation
  - compact-with-summary
  - pinned messages
- Memory paths:
  - `~/.arbe/memory/global/memory.md`
  - `~/.arbe/memory/projects/<project-id>/memory.md`

### Exit Criteria
- Config can switch strategy at runtime (next turn).
- Token budget overflow handled deterministically.

---

## Phase 4 — Tools + Approval Gate (Week 5)
### Scope
- Implement tool invocation flow with user safety gate.

### Deliverables
- Tool registry and typed invocation model.
- Policy engine:
  - always_prompt
  - allowlist_auto
  - denylist_block
  - dry_run_only
- Runtime pause/resume for approval.
- Tool execution logging + audit fields.

### Exit Criteria
- Tool call cannot execute without policy decision.
- User decision unblocks loop correctly.
- Full invocation trace persisted.

---

## Phase 5 — Skills, Hooks, MCP Baseline (Week 6)
### Scope
- Add extensibility mechanisms.

### Deliverables
- Skill loading from global/project/session scopes.
- Hook lifecycle registration/execution with timeout isolation.
- MCP server config load + capability exposure as tools.

### Exit Criteria
- Sample skill modifies instruction context.
- Sample hook observes/annotates events.
- One MCP server tool appears in unified registry.

---

## Phase 6 — TUI MVP (Week 7)
### Scope
- Build minimal but solid chat UI integrated with runtime events.

### Deliverables
- Transcript pane + input line.
- Streaming token render.
- Tool approval modal.
- Status bar (provider/model/phase/session id).
- Resume session selector.

### Exit Criteria
- User can complete an end-to-end session from TUI, including tool approval.

---

## Phase 7 — Hardening & Release Candidate (Week 8)
### Scope
- Stabilization, tests, docs, performance tuning.

### Deliverables
- Integration test suite:
  - provider swap
  - context strategy swap
  - tool approval flow
  - session recovery
- Benchmark scenarios for turn latency and stream smoothness.
- Release docs and migration notes (if needed).

### Exit Criteria
- All acceptance criteria in harness and TUI specs satisfied.
- v1 tag candidate ready.

---

## 4. Backlog by Crate

## `arbe-core`
- Domain models (Session, Turn, Message, ToolCall, Events)
- State machine and transition guards
- Error taxonomy

## `arbe-runtime`
- Orchestration glue
- Event bus
- Command handlers from UI/CLI

## `arbe-storage`
- FS adapters for config, sessions, memory, logs
- Atomic writes and safe append APIs

## `arbe-providers`
- Trait + adapters (OpenAI/Ollama first)
- Request/response normalization
- Streaming abstraction

## `arbe-memory`
- Context builder
- Truncation/compaction policies
- Pinning and summaries

## `arbe-tools`
- Tool schema and invocation API
- Policy/approval service
- Execution adapters + sandbox boundary (future tightening)

## `arbe-skills`
- Manifest parser
- Scope resolution and merge policy

## `arbe-hooks`
- Hook registration
- Execution ordering/timeouts/isolation

## `arbe-mcp`
- Server discovery/config parse
- Tool mapping bridge

## `arbe-tui`
- Event-driven rendering
- Input/action dispatch
- Modal workflow for approvals/errors

---

## 5. Suggested Config Schema (v1 draft)

```toml
[profile.default]
provider = "openai"
model = "gpt-5-mini"
temperature = 0.2
max_tokens = 4096

[memory]
strategy = "compact_summary"
context_budget_tokens = 64000
pinned_turns = 6

[tools]
policy = "always_prompt"
allowlist = []
denylist = []

[hooks]
enabled = true
timeout_ms = 500

[mcp]
enabled = true
servers_file = "~/.arbe/mcp/servers.toml"
```

---

## 6. Risk Register

1. **Provider divergence in tool-calling formats**
   - Mitigation: normalized internal AST + adapter-specific codecs.

2. **Context budget instability**
   - Mitigation: deterministic budget accounting + fallback truncation.

3. **TUI responsiveness under streaming**
   - Mitigation: bounded render batching and non-blocking event handling.

4. **Cross-platform process/tool execution differences**
   - Mitigation: platform abstraction layer + CI matrix tests for tool execution.

5. **Extensibility causing complexity drift**
   - Mitigation: trait contracts + strict module boundaries + architecture tests.

---

## 7. Definition of Done (v1)
- Meets all acceptance criteria in:
  - `docs/v1-overall-design.md`
  - `docs/v1-harness-spec.md`
  - `docs/v1-tui-spec.md`
- CI green on Linux/macOS/Windows.
- End-to-end TUI demo with:
  - streaming response
  - approval-gated tool execution
  - resumable session
- Documentation sufficient for new contributors to add:
  - a new provider adapter
  - a new context strategy
  - a new tool executor
