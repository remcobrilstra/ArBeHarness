# ArBeHarness

A high-performance, model-agnostic Rust agent harness with a lightweight TUI. The harness (core runtime, providers, memory, tools, skills, hooks, MCP) is the product; the TUI is a thin, decoupled visualization/control layer on top of it.

Full specs live in `/docs` — read them before making architectural decisions, don't re-derive from scratch:
- `docs/v1-overall-design.md` — architecture, workspace layout, module responsibilities
- `docs/v1-harness-spec.md` — functional/non-functional requirements, core trait signatures, persistence + error taxonomy
- `docs/v1-implementation-plan.md` — phase-by-phase delivery plan, per-crate backlog, config schema, risk register
- `docs/v1-tui-spec.md` — TUI requirements, event contract, keybindings

## Current state

Phase 0 (workspace + CI skeleton) is done: the `crates/arbe-*` workspace exists, each crate compiles, and each non-`arbe-core` crate defines only its shared trait/type contract from the spec docs (no behavior yet — that's what each later phase fills in). `arbe-core` has real domain types (`SessionMeta`, `Turn`, `Message`, `ToolInvocation`), the `LoopPhase` state enum, the `RuntimeEvent`/`RuntimeCommand` contract, and the error taxonomy. `arbe-runtime` has a working `EventBus` (tokio broadcast). `arbe-storage` has `~/.arbe/` path resolution only — no read/write yet. CI matrix (fmt/clippy -D warnings/test on ubuntu/macos/windows) is in `.github/workflows/ci.yml`.

Phase 1 (loop skeleton + persistence) is done: `arbe_core::LoopMachine` enforces the legal `LoopPhase` transition graph from the design doc (including the tool-call and direct-response branches); `arbe_storage::SessionStore` persists `meta.json` (atomic write), append-only `turns.jsonl`/`events.jsonl`, and supports create/load/resume/list, with tests covering forced-interruption recovery. `SessionStore` takes an explicit root (`with_root`) rather than always reading the global `~/.arbe/` — use that in tests instead of mutating `ARBE_HOME`, since env vars are process-global and races across parallel tests.

Phase 2 (provider abstraction + first adapters) is done: `arbe-providers` has `OpenAiProvider` and `OllamaProvider`, both implementing `ModelProvider` (non-streaming `infer` + streaming `infer_stream` over SSE / newline-delimited JSON respectively), plus `build_provider(name, api_key, base_url)` so switching providers is a config change (implementation plan Phase 2 exit criterion). HTTP/parsing logic is split into pure, network-free functions (`build_request_body`, `parse_response`, `parse_stream_payload`/`parse_stream_line`, `SseDecoder`, `map_http_error`/`map_transport_error`) so they're unit-testable without a live API or mock server — there is currently no integration test that hits a real OpenAI/Ollama endpoint.

Phase 3 (context & memory v1) is done: `arbe-memory` has a deterministic token estimator (`estimate_tokens`, ~4 chars/token — not a real tokenizer, but reproducible without a provider-specific one), the two required strategies (`TruncationStrategy`, `CompactWithSummaryStrategy`) sharing one selection rule (`truncation::select_kept`: pinned turns always kept, most recent unpinned kept until budget runs out), and a `ContextPipeline` assembling system/global/skill instructions → history → memory notes → user turn per overall design §5.2, deducting fixed costs from the budget before handing the remainder to the strategy. `arbe-storage::memory_files` reads `<home>/memory/global/memory.md` and `<home>/memory/projects/<id>/memory.md`, returning `Ok(None)` (not an error) when absent; the `*_at(root, ...)` variants take an explicit root for the same reason `SessionStore` does (no `ARBE_HOME` races across parallel tests).

Phase 4 (tools + approval gate) is done: `arbe-tools::ToolRegistry` maps tool name -> `Arc<dyn ToolExecutor>` (this is also where MCP-exposed tools land later, per overall design §4.4 — same registry, no separate MCP path). `StandardApprovalPolicy` implements the four harness-spec FR-4 modes against a `PolicyOutcome` (`AutoApprove`/`AutoDeny`/`RequiresPrompt`) — deliberately not `arbe_core::ApprovalDecision` directly, since only a human decision can resolve `RequiresPrompt` and the policy alone can't produce one. `gate::execute_gated` is the single choke point: it looks up the tool, asks the policy, and — if the policy says `RequiresPrompt` and no `human_decision` was passed — returns `GatedOutcome::PendingApproval` instead of executing, so the runtime loop can pause at `LoopPhase::ToolApproval` and re-call it with the human's decision once one arrives. No code path in this crate reaches `ToolExecutor::execute` except through `execute_gated`.

Not wired yet: `arbe-runtime` doesn't yet drive `LoopMachine` through an actual turn (that needs `execute_gated` called from within the loop, plus a way to actually pause and receive the human decision asynchronously — likely via the `EventBus`/a command channel). That end-to-end wiring, plus skills/hooks/MCP, is Phase 5 onward — see `docs/v1-implementation-plan.md` and the tracked tasks.

## Non-negotiable architecture rules

- `arbe-core` must never depend on `arbe-tui`. The TUI consumes runtime events/commands only — no agent decision logic in the TUI crate.
- Memory strategies, provider adapters, skill registries, tool executors, and approval policies are all trait-based and swappable via config, not code changes.
- Tool execution always goes through the approval gate (`ApprovalPolicy`) — no side-effecting tool call bypasses it, even in early scaffolding/test code.
- All persistent state lives under `~/.arbe/` (config, sessions, memory, skills, instructions, mcp, logs). Use atomic writes (temp file + rename) for config/metadata; JSONL append-only for turns/events.
- Cross-platform from day one: avoid OS-specific path/process assumptions (Linux/macOS/Windows CI matrix is part of Definition of Done).

## Commands

```bash
cargo fmt        # required clean before commit
cargo clippy -- -D warnings   # required clean before commit
cargo check
cargo test
cargo run
```

## Working conventions

- Prefer adding to the existing crate/module structure implied by the current phase over introducing new crates ahead of schedule.
- Config-first: new strategies/behaviors should be selectable via config (see the `[profile]`/`[memory]`/`[tools]`/`[hooks]`/`[mcp]` schema draft in the implementation plan), not hardcoded branches.
- When a phase's exit criteria aren't met yet, say so explicitly rather than marking it done.
