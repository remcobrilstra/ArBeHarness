# ArBeHarness v1 — Acceptance Criteria Status

Honest pass/fail against the acceptance criteria in `v1-harness-spec.md` §7 and `v1-tui-spec.md` §10, as of the end of the Phase 0–7 implementation pass. "Done" means verified (tests or a real run); "Partial" means real but incomplete; caveats are called out explicitly rather than glossed over.

## Harness spec (§7)

1. **End-to-end chat session works with at least two providers (one hosted, one local).**
   Partial. `OpenAiProvider` and `OllamaProvider` both implement `ModelProvider` (streaming + non-streaming); `Agent::submit_message` drives a real end-to-end turn through either one via `build_provider(name, ...)`. Request/response parsing and error mapping are unit-tested for both. **Not verified**: an actual network round-trip against a live OpenAI or Ollama endpoint — this environment has no reachable Ollama server and no API key. The TUI does launch with Ollama selected by default (confirmed via a captured run), but no message was sent through a live connection.

2. **Tool calls are parsed, approval-gated, executed, and logged.**
   Partial. Approval-gating (`StandardApprovalPolicy`), execution (`execute_gated`, the single choke point), and logging (audit fields on `ToolInvocation`/`ToolResult`, `RuntimeEvent`s) are all real and tested. `arbe-tools::builtin` now ships a real, tested tool set — `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `execute` — sandboxed to `RuntimeConfig::project_dir` via `path_guard::resolve_within_root` and registered automatically by every `Agent`. **Gap**: there is still no automatic tool-call *parsing* from a model's response — `ModelResponse`/`TokenChunk` only carry plain text, so no provider currently surfaces structured tool calls. `Agent::propose_tool_call`/`resolve_tool_call` (wired to the TUI's `/tool <name> <json>` command) exercise the identical approval/execution path as a stand-in, now against the real builtin tools rather than a demo echo tool. Closing this gap is future work: teaching the OpenAI/Ollama adapters to surface `tool_calls` in `ModelResponse` and `InterpretOutput` to build `ToolInvocation`s from them automatically.

3. **Memory strategies can be switched via config without code changes.**
   Done. `RuntimeConfig::memory_strategy` (`"truncation"` / `"compact_summary"`) selects the strategy in `Agent::assemble`; `agent::tests::memory_strategy_is_swappable_via_config_alone` proves the same history + budget produces visibly different output (a compaction marker) depending only on which strategy is selected.

4. **Skills and hooks execute in the expected lifecycle order.**
   Partial. `HookRegistry` runs hooks in registration order with per-hook timeout+panic isolation (5 tests, including an intentionally-panicking hook). `Agent::submit_message` invokes hooks at `BeforeModelCall`/`AfterModelCall`/`OnTurnComplete`. Skills are loaded from `~/.arbe/skills/` (global scope) and folded into the context pipeline's instructions via `arbe_skills::load_dir`/`merge_skills`. **Gap**: only the global skill scope is wired into `Agent` — session-local and project-local skill directories are not yet surfaced through `RuntimeConfig`, and hooks aren't invoked at every phase listed in the harness spec (`BeforeContextAssembly`, `BeforeToolExecute`, `AfterToolExecute`, `OnError` are defined but not called from `Agent` yet).

5. **Session recovery works after forced interruption.**
   Done. `arbe_storage::SessionStore` persists atomically and appends turns as they happen (no buffering), so a killed process loses at most the in-flight turn. `agent::tests::session_recovers_after_a_forced_interruption` drops an `Agent` mid-session without calling `close()`, then reconstructs one from the same store and confirms history and the next turn index are both correct, and that a subsequent turn appends correctly.

6. **TUI can consume runtime events without direct core coupling.**
   Done. `arbe-tui`'s only dependency is `arbe-runtime`; it reaches `arbe_core`/`arbe_tools` types exclusively through `arbe_runtime`'s re-exports. Verified by `cargo tree`-level dependency shape (no direct `arbe-core`/`arbe-tools`/etc. entries in `arbe-tui/Cargo.toml`) and by a real captured TUI run.

7. **CI passes on Linux/macOS/Windows with lint/tests/build.**
   Partial. `.github/workflows/ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on all three OSes on every push/PR. All three checks pass locally on Windows (this development environment). **Not verified**: the workflow has never actually run on GitHub Actions, since nothing has been pushed to a remote — that requires an explicit push, which wasn't requested.

## TUI spec (§10)

1. **User can run chat session entirely from TUI with streaming responses.** Done for the mechanism (Agent calls run on spawned tasks, `ModelStreamChunk` events update the transcript live without blocking input) — not interactively verified end-to-end by a human, since this environment has no attached TTY to drive keypresses.
2. **Tool calls always pass through visible approval flow (per policy).** Done for the manual path (`/tool`, now backed by the real builtin tool set); not true yet for model-initiated tool calls, per gap #2 above.
3. **TUI can resume a previous session from persisted storage.** `Agent::resume` exists and is tested at the `Agent` level; the TUI binary itself doesn't yet have a session picker UI (TUI-FR-3 "resume previous session" control) — currently always calls `Agent::create`. Small, well-scoped gap.
4. **Errors are surfaced without crashing TUI.** Done — provider/tool errors surface as a status-line banner (`app.status_message`), not a panic; the render loop keeps running.
5. **UI remains responsive during model streaming and tool execution.** Done — see the concurrency design above (spawned tasks + channel + non-blocking event draining).
6. **No direct dependency from core harness crate to TUI crate.** Done — enforced by the Cargo dependency graph itself (`arbe-core` has no dependents outside the workspace's own crates, and none of them depend on `arbe-tui`).

## Summary

164 tests passing across the workspace, `cargo fmt --check` and `cargo clippy -D warnings` clean. Five real, scoped gaps remain for a true v1.0 release, all called out above: live-provider network verification, model-initiated tool-call parsing (the builtin tools themselves are real and tested; only automatic invocation from a model's response is missing), session-local/project-local skill scopes + full hook phase coverage, a TUI session picker, and an actual GitHub Actions run. None of these were faked or glossed over — they're the honest remainder after Phases 0–7 plus the builtin tool set added afterward.
