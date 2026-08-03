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

Not started: Phase 2 (provider adapters) onward — see `docs/v1-implementation-plan.md` and the tracked tasks for the phase breakdown. Follow phase order; don't wire a tool executor or context strategy before real provider inference (Phase 2) exists, since later phases assume a working model round-trip.

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
