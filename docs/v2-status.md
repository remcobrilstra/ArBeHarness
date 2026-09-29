# ArBeHarness v2 — Acceptance Status

Honest pass/fail against every exit criterion in [`v2-implementation-plan.md`](v2-implementation-plan.md), as of **2026-09-29** (branch `v2`). "Met" means verified by a test or a real run, with the evidence named. Anything short of that is "Open", with what's missing. Nothing below is marked met on the strength of code review alone.

The v2.0 release bar (plan, risk register): **P0–P5 + P6.1–P6.3 + P7.**

## Summary

| Phase | Exit criteria | What's open |
|---|---|---|
| P0 Housekeeping | Met | — |
| P1 Core types | Met | — |
| P2 Provider layer | **Open** | Live runs against api.openai.com and Anthropic (no keys available here) |
| P3 Agent loop | **Open** | An interactive TUI session checked by a human at a real terminal |
| P4 Config, profiles, extensions | Met | — |
| P5 Context management | Met | — |
| P6.1–P6.3 Embedding, headless, subagents | Met | — (P6.4–P6.8 are outside the release bar) |
| P7 Verification & release | **Open** | Live runs above; `v0.2.0` tag and binaries |

**Tests:** 471 passing, 21 `#[ignore]`d live tests; `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` and `cargo deny check` clean — on Windows locally, and in GitHub Actions on Linux, macOS and Windows (run 36579578868, commit `2823e18`, all green).

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

## P3 — Agent loop v2 — Open

- *Agent-level tests (scripted provider) for multi-round tool use, parallel tools with ordering, cancel mid-stream and mid-tool, crash-then-resume mid-turn, every loop guard:* met — `crates/arbe-runtime/src/agent/tests.rs`.
- *No agent-module function over ~150 lines:* met — the longest is `turn::model_loop` at 152 lines (measured 2026-09-29).
- *Interactive TUI run verified by a human at a real terminal:* **open.** This environment has no interactive terminal. The TUI's logic is unit-tested (22 tests, including rendering/scroll consistency) and its layout was confirmed by a captured run, but nobody has typed into it since v2's changes. Worth checking in particular: streaming a reply, the approval dialog (including a subagent's approval), `Esc` to cancel, `Ctrl+T` thinking, `Ctrl+R` resume, and `--prompt`.

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

Outside the release bar and not started: P6.4 background execution, P6.5 `ask_user`, P6.6 plan mode, P6.7 web tools, P6.8 observability.

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

## Known limitations (by design or deferred)

- Subagents use the parent's configuration (no per-call profile), don't connect MCP servers, and their token use isn't added to the parent's total.
- No cost display (the model catalog has no prices).
- `events.jsonl` isn't written (the event stream is available live, via the TUI, `--print --output json` and `--headless`).
- Small local models (3B) are unreliable at tool use: `llama3.2:3b` calls `remember` on trivia, and `qwen2.5-coder:3b` sometimes repeats a tool call. The harness handles both safely (approval gate, repeated-call guard) but can't make them good agents.
