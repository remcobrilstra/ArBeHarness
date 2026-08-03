# ArBeHarness v1 — Harness Specification

## 1. Objective
Define the functional and technical specification for the v1 **agent harness core** of ArBeHarness.

The harness must be:
- Rust-native and framework-independent for agent loop control.
- Model-agnostic across OpenAI/Anthropic/xAI/Mistral/Ollama.
- Extensible with MCP, skills, hooks, and memory strategy plugins.
- Safe by default for tool execution (approval gate).
- Persisted under `~/.arbe/`.
- Cross-platform (Linux/macOS/Windows).

---

## 2. Functional Requirements

### FR-1 Session Lifecycle
- Create, load, resume, and close sessions.
- Persist each turn (user input, assistant output, tool actions, errors, metadata).
- Support interrupted-session recovery.

### FR-2 Provider Abstraction
- Unified provider interface for chat completion + streaming.
- Provider capability flags:
  - streaming support
  - tool call support
  - JSON/structured output support
  - max context size metadata
- Normalized provider errors.

### FR-3 Agent Loop
- Explicit state machine loop:
  1. Input intake
  2. Context assembly
  3. Inference request
  4. Output interpretation
  5. Optional tool approval
  6. Tool execution
  7. Follow-up inference
  8. Persistence + emit events
- Must support cancellation, timeout, retry policy.

### FR-4 Tools and Approval
- Tool registry with typed signatures.
- Tool execution only after policy + user approval check (unless policy permits auto-allow).
- Policy modes:
  - always_prompt
  - allowlist_auto
  - denylist_block
  - dry_run_only
- Every invocation audited in session logs.

### FR-5 Memory and Context
- Multi-layer memory:
  - working context
  - session history
  - `memory.md` notes
  - future long-term providers
- Context strategies configurable at runtime.
- v1 strategies:
  - truncation
  - compaction-with-summary
  - pinned-message retention

### FR-6 Skills
- Load skills from markdown manifests.
- Merge from scopes:
  1) session-local
  2) project-local
  3) global `~/.arbe/skills`
- Skill selection can be explicit and optionally rule-based.

### FR-7 Hooks
- Hook points:
  - before_context_assembly
  - before_model_call
  - after_model_call
  - before_tool_execute
  - after_tool_execute
  - on_error
  - on_turn_complete
- Hook failures isolated from core flow unless configured strict.

### FR-8 MCP
- Read MCP server definitions from config.
- Connect and expose MCP tools through same internal tool API.
- Handle unavailable servers gracefully.

### FR-9 Configuration
- Hierarchical config precedence:
  1. CLI flags
  2. session overrides
  3. profile config
  4. global config
  5. defaults
- Hot-reload optional for selected runtime settings in v1 if low complexity.

### FR-10 Observability
- Structured logs with correlation IDs:
  - session_id
  - turn_id
  - provider request id (if present)
- Event stream for UI and debug tooling.

---

## 3. Non-Functional Requirements

### NFR-1 Performance
- Streaming-first output delivery.
- Low allocation churn in loop hot path.
- Predictable latency under typical turn sizes.

### NFR-2 Reliability
- Fault isolation between provider/tool/hook failures.
- Bounded retries and backoff for transient errors.
- Safe persistence with atomic writes.

### NFR-3 Portability
- Identical feature behavior across Linux/macOS/Windows.
- File/path/process behavior validated in CI matrix.

### NFR-4 Security & Safety
- No tool side effects without policy/approval.
- Secrets not logged in plaintext.
- Provider keys sourced from env/config indirection.

### NFR-5 Maintainability
- Trait-driven module contracts.
- Unit + integration tests for loop, memory, tools, providers.
- Strict lint + formatting gates in CI.

---

## 4. Core Interfaces (Conceptual)

```rust
trait ModelProvider {
    fn capabilities(&self) -> ProviderCapabilities;
    async fn infer(&self, req: ModelRequest) -> Result<ModelResponse, ProviderError>;
    async fn infer_stream(&self, req: ModelRequest) -> Result<TokenStream, ProviderError>;
}

trait ContextStrategy {
    fn build_context(&self, input: ContextInput) -> ContextOutput;
}

trait ToolExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError>;
}

trait ApprovalPolicy {
    fn decide(&self, invocation: &ToolInvocation, ctx: &ApprovalContext) -> ApprovalDecision;
}
```

---

## 5. Persistence Specification (`~/.arbe/`)

```text
~/.arbe/
  config/
    config.toml
    profiles/
  providers/
    credentials.toml          # or secure references
  sessions/
    <session-id>/
      meta.json
      turns.jsonl
      events.jsonl
      artifacts/
  skills/
    *.md
  instructions/
    *.md
  memory/
    global/memory.md
    projects/<project-id>/memory.md
  mcp/
    servers.toml
  logs/
    runtime.log
```

Data format requirements:
- `turns.jsonl`: append-only for robustness.
- `meta.json`: session-level metadata and status.
- Atomic file writes via temp + rename for critical files.

---

## 6. Error Taxonomy
- `ProviderError`: auth/rate_limit/timeout/invalid_request/internal
- `ToolError`: validation/approval_denied/runtime_failure/timeout
- `MemoryError`: parse_failure/budget_failure/store_unavailable
- `ConfigError`: invalid_schema/missing_value/conflict
- `HookError`: timeout/panic/contract_violation

All user-facing errors should include:
1) concise reason
2) likely fix
3) optional verbose trace pointer

---

## 7. v1 Acceptance Criteria
1. End-to-end chat session works with at least two providers (one hosted, one local).
2. Tool calls are parsed, approval-gated, executed, and logged.
3. Memory strategies can be switched via config without code changes.
4. Skills and hooks execute in the expected lifecycle order.
5. Session recovery works after forced interruption.
6. TUI can consume runtime events without direct core coupling.
7. CI passes on Linux/macOS/Windows with lint/tests/build.
