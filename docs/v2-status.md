# ArBeHarness v2 — Acceptance Status

Honest pass/fail against every exit criterion in [`v2-implementation-plan.md`](v2-implementation-plan.md), as of **2026-09-29** (branch `v2`). "Met" means verified by a test or a real run, with the evidence named. Anything short of that is "Open", with what's missing. Nothing below is marked met on the strength of code review alone.

The v2.0 release bar (plan, risk register): **P0–P5 + P6.1–P6.3 + P7.**

## Summary

| Phase | Exit criteria | What's open |
|---|---|---|
| P0 Housekeeping | Met | — |
| P1 Core types | Met | — |
| P2 Provider layer | **Open** | Live runs against api.openai.com and Anthropic (no keys available here) |
| P3 Agent loop | Met | — |
| P4 Config, profiles, extensions | Met | — |
| P5 Context management | Met | — |
| P6.1–P6.3 Embedding, headless, subagents | Met | — (P6.4–P6.8, outside the release bar, are done too) |
| P7 Verification & release | **Open** | Live runs above; `v0.2.0` tag and binaries |

**Tests:** 495 passing on Windows (497 on Linux, which runs two Unix-only tests), 24 `#[ignore]`d live tests; `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` and `cargo deny check` clean. Last full GitHub Actions run on Linux, macOS and Windows: run 36579578868, commit `2823e18`, all green; later commits verified locally on Windows and in a Linux container.

## P0 — Housekeeping & quick correctness fixes — Met

- *Session approvals covered by gate + policy tests:* met — `arbe-tools` gate/policy/`session_approvals` tests.
- *An Ollama model calls a builtin tool end-to-end:* met, live — `crates/arbe-runtime/tests/live_agent.rs` on `qwen2.5-coder:3b` (`read_file`, `edit_file`), 2026-09-28. This needed a fix found by that run: qwen writes tool calls as JSON text (`arbe-providers/src/text_tool_calls.rs`).
- *fmt / clippy / test clean:* met.

## P1 — Core types v2 — Met

- *All crates on the new types, nothing stubbed:* met.
- *`ResponseAccumulator` property-tested for arbitrary tool-JSON fragmentation:* met — `arbe-providers/src/accumulator.rs` tests.
- *A v1 `turns.jsonl` fixture loads through the v2 reader:* met — `arbe-storage` tests.

## P2 — Provider layer v2 — Open

- *The same turn (with a tool call) runs on OpenAI, Anthropic and Ollama by config switch only:* **partly.**
  - Ollama: live (`qwen2.5-coder:3b`, `llama3.2:3b`).
  - OpenAI's wire format: live through `openai_compatible` against xAI (`grok-4.7`, `grok-4.20-0309-reasoning`) — the same adapter code (SSE, streamed tool-call deltas, `reasoning_content`) — but **not against api.openai.com** itself.
  - Anthropic: **not run live.** Covered by fixture tests and by `crates/arbe-providers/tests/http.rs`, which drives the real adapter over HTTP (streaming, tool calls, errors, `Retry-After`, truncation).
- *All three pass fixture tests:* met — plus the HTTP-level suite above for all three.
- *A live smoke test passes for every provider whose credentials are available:* met for what's available (Ollama, xAI). Results are in the plan's P7.2 notes.

**To close:** run `cargo test -p arbe-providers --test live -- --ignored --nocapture` with `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` set (or trigger `.github/workflows/live.yml` with those secrets).

## P3 — Agent loop v2 — Met

- *Agent-level tests (scripted provider) for multi-round tool use, parallel tools with ordering, cancel mid-stream and mid-tool, crash-then-resume mid-turn, every loop guard:* met — `crates/arbe-runtime/src/agent/tests.rs`.
- *No agent-module function over ~150 lines:* met — the longest is `turn::model_loop` at 152 lines (measured 2026-09-29).
- *Interactive TUI run verified by a human at a real terminal:* met — the maintainer used the TUI against grok-4.7 (including the Ctrl+P profile switch) and accepted it on 2026-09-29. The TUI's logic is also unit-tested (rendering/scroll consistency, dialogs, profile picker).

## P4 — Config, profiles & extension wiring — Met

- *Switching `coding` ↔ `general` changes tools/prompt/policy with no code change:* met — `the_general_profile_agent_only_has_its_allowed_tools` and config tests.
- *A real MCP server's tools are callable by the model:* met — against the official `server-everything`, through the agent (scripted) and, live, by `grok-4.7` (`uses_tools_from_an_mcp_server`); plus the in-tree fixture server tests.
- *A command hook can veto a tool call:* met — `command_hooks_can_veto_tool_calls_and_their_failures_are_reported`, and live: `a_hook_can_veto_a_models_command` (grok-4.7).

## P5 — Context management v2 — Met

- *A scripted 200-turn session with large tool outputs stays within budget without breaking tool-use/result pairing:* met — `a_200_turn_session_with_large_tool_output_stays_within_budget` (checks all 400 requests).
- *Compaction is persisted and survives resume:* met — `a_compaction_survives_resume`; and live, `compaction_keeps_what_matters` (the model recalled a fact after compaction).

## P6 — Multi-purpose & embedding (release bar: P6.1–P6.3) — Met

- *Headless mode driven end-to-end by an integration test that spawns the binary:* met — `tests/cli.rs::headless_mode_speaks_json_rpc_over_stdio`; plus in-process protocol tests, and a 24-check client run against xAI.
- *A subagent test shows context isolation and approval routing:* met — `a_subagent_works_in_its_own_context_and_its_approvals_reach_the_parent`, `cancelling_the_parent_cancels_a_waiting_subagent`; live, `delegates_research_to_a_subagent`.
- *The minimal embedder example compiles in CI:* met — `crates/arbe-runtime/examples/embed.rs` builds under `clippy --all-targets` in CI on all three OSes, and ran against a local model.

Outside the release bar: P6.4 background processes, P6.5 `ask_user`, P6.7 web tools and P6.8 observability are done and were verified live on grok-4.7 (`web_search` only against a mock server; OpenTelemetry deferred). P6.6 plan mode is done as the first of general session modes: agent-level tests for refusal, approval (mode ends mid-turn) and decline, persistence across resume; verified live on grok-4.7 both ways.

## P7 — Verification, hardening & release — Open

| Item | Status | Evidence / what's missing |
|---|---|---|
| P7.1 HTTP mock tests | Met | `crates/arbe-providers/tests/http.rs`, 10 tests × 3 wire formats. Found and fixed: silently accepting a truncated stream; 500/502 not retried. |
| P7.2 Live smoke tests | Partly | Tests exist for every provider and at agent + CLI level; run live on Ollama and xAI. Open: OpenAI and Anthropic runs; `.github/workflows/live.yml` never run. |
| P7.3 Reference MCP server | Met | `mcp-fixture-server` + 8 integration tests. |
| P7.4 Gate enforcement | Met | `ToolContext` is issued only by the gate; `compile_fail` doctests. |
| P7.5 JSONL read performance | Met (by measurement) | Resume of 5,000 turns / 46 MB reads in 66 ms; no change needed. |
| P7.6 Benchmarks | Met | `cargo bench` in arbe-memory, arbe-storage, arbe-providers; results in the plan. |
| P7.7 CI actually running | Met | First run on GitHub (2026-09-29) failed on Linux/macOS: a real bug — `trusted_projects` entries with `..` never matched on Unix (fixed in `2823e18`, reproduced and verified in a Linux container). Second run green on all three OSes + `cargo-deny`. |
| P7.8 Docs & release | **Open** | This file, README and CHANGELOG are written. `LICENSE` added: proprietary, all rights reserved, until a license is chosen. Open: the `v0.2.0` tag and prebuilt binaries. |

## Pending work

Everything not done yet, in one place. Nothing here blocks using the harness today.

**Needs the maintainer**

| Item | What's needed |
|---|---|
| P2 / P7.2 live runs on OpenAI and Anthropic | API keys. The tests exist (`cargo test -p arbe-providers --test live -- --ignored`, `live_agent`, `.github/workflows/live.yml` with secrets); OpenAI's wire format already runs live through `openai_compatible` against xAI. |
| P7.8 `v0.2.0` tag and prebuilt binaries | A release workflow (not written yet) and the go-ahead to tag. |
| License | `LICENSE` is proprietary (all rights reserved) until one is chosen. |
| Desktop control | Review of [`desktop-control-design.md`](desktop-control-design.md) and two decisions: allow BSD-2-Clause in `deny.toml` (for `xcap`) or write our own capture code; on Linux, one binary that needs `libxkbcommon` or two builds. |

**Features not built**

| Item | State |
|---|---|
| Custom modes | Modes are data (`agent/modes.rs`), but only the built-in `default` and `plan` exist; `[modes.<name>]` in config isn't read yet. |
| Desktop control | Designed only (see above). |
| OpenTelemetry export (P6.8) | Deferred; the daily log file covers local debugging. |
| `events.jsonl` | Not written; events are available live (TUI, `--print --output json`, `--headless`). |
| Subagent gaps | Subagents use the parent's configuration (no per-call profile), don't connect MCP servers, and their tokens and cost aren't added to the parent's totals. |
| Smaller ideas | [`todo.md`](todo.md): nested project instruction files, MCP schema lookup before use, output-formatting conventions. A punch list, not a commitment. |

**Built but not verified against the real thing**

| Item | What's missing |
|---|---|
| `web_search` | Tested only against a mock server; no Brave/Tavily/SearXNG account here. |
| `.github/workflows/live.yml` | Never run (needs repository secrets). |

## Known limitations (by design or deferred)

- No built-in model prices (they go stale): costs appear only for models whose prices you set in `[[models]]`.
- Small local models (3B) are unreliable at tool use: `llama3.2:3b` calls `remember` on trivia, and `qwen2.5-coder:3b` sometimes repeats a tool call. The harness handles both safely (approval gate, repeated-call guard) but can't make them good agents.
