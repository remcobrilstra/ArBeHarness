# ArBeHarness v2 — Implementation Plan

**Goal:** turn the v1 product core into a high-end, multi-purpose agent harness — without a rewrite.
**Strategy:** rebuild the *center* (message model, provider contract, agent loop) and keep the *leaves* (storage, `path_guard`, builtin tools, approval gate, SSE/UTF-8 decoding, MCP framing, hook isolation, most of the TUI).
**Created:** 2026-09-28 — derived from the full-codebase review of that date.

The v1 docs (`v1-*.md`) remain the reference for anything this plan doesn't change. The architecture rules in `CLAUDE.md` still apply unchanged: core never depends on TUI, everything pluggable is trait-based and config-selectable, every tool call goes through the approval gate, all state lives under `~/.arbe/`, and CI must pass on Linux, macOS and Windows.

---

## Status overview

Update this table and the task checkboxes as work lands. Status values: `Not started` · `In progress` · `Done` · `Blocked`. A phase is `Done` only when every exit criterion is verified (by tests or a real run), not just implemented.

| Phase | Title | Status | Tasks done | Notes |
|---|---|---|---|---|
| P0 | Housekeeping & quick correctness fixes | Done | 6 / 6 | 223 tests, fmt/clippy clean. P0.3 not verified against a live Ollama server (none available) |
| P1 | Core types v2 (content blocks, events, cancellation, persistence schema) | Done | 7 / 7 | 265 tests, fmt/clippy clean. Pulled forward parts of P2.2/P2.3 (adapters on the new trait, streamed tool calls, usage), P3.2 (streaming every round) and P3.7 (tool errors go back to the model) |
| P2 | Provider layer v2 | In progress | 9 / 9 | All implemented and fixture-tested (296 tests). **Not Done yet:** the exit criterion needs live runs, and there are no API keys or Ollama server here — run `cargo test -p arbe-providers --test live -- --ignored --nocapture` with keys set |
| P3 | Agent loop v2 | In progress | 8 / 10 | 322 tests, fmt/clippy clean. Open: P3.7 image/block tool results, P3.10 thinking view + live tool-arg rendering; interactive TUI run by a human still pending |
| P4 | Config, profiles & extension wiring | In progress | 3 / 9 | Config files, profiles and registry-derived tool specs done; MCP wiring next |
| P5 | Context management v2 | Not started | 0 / 6 | Depends on P3 |
| P6 | Multi-purpose & embedding | Not started | 0 / 8 | Depends on P4 |
| P7 | Verification, hardening & release | In progress | 0 / 8 | Live smoke tests exist (P7.2, partial); CI dispatch workflow still open |

**Current focus:** P4.4/P4.5 (MCP wiring and client robustness)
**Last updated:** 2026-09-28 · test count: 343

### Progress log

Newest first. One entry per working session: what landed, and anything the next session needs to know.

- **2026-09-28 — P4.1–P4.3 done; user guide adopted.** `docs/user-guide.md` (written by someone else) is now committed and kept in sync — CLAUDE.md makes that a working convention. TOML config (`~/.arbe/config/config.toml`, `<workdir>/.arbe/config.toml`) with strict parsing, layered under env vars; profiles (`coding`, `general`, user-defined) carrying a tool allow-set and prompt template; tool specs derived from each tool's argument type via `schemars`, so tools added at runtime are offered to the model too. Also: `--profile`; invalid env numbers are now errors instead of being silently ignored; Enter during a turn no longer adds an unsent message to the transcript; quitting waits briefly for a cancelled turn to persist. 322 → 343 tests. Next: P4.4.
- **2026-09-28 — P3 mostly complete.** `Agent` rewritten as `agent/` modules with a `&self` API (no outer lock → cancel and approve work mid-turn; `Busy` on concurrent turns); full tool trace persisted via an in-flight write-ahead log and replayed, with crash recovery; turn-granular history trimming; split gate (`authorize` → `Authorized::execute`) with sequential approval + parallel execution; loop guards with explicit stop reasons; all hook phases with typed payloads (`BeforeToolExecute` can rewrite/veto); manual `/tool` on the same path; TUI on `Arc<Agent>` with `Esc` to cancel. Found and fixed: dangling-call closure matched call ids globally, which breaks with servers that reuse ids (`call_0`) — now positional. Also: denial no longer ends the turn with a canned apology; the model sees it and responds. 296 → 322 tests. Next: P4.1.
- **2026-09-28 — P2 implemented (live verification pending).** Retry/backoff with `Retry-After` + cancellable waits, surfaced as `ProviderRetrying` events; connect/idle-read timeouts; Anthropic adapter (thinking + signatures, redacted thinking, prompt caching, role merging); model catalog; provider registry; `openai_compatible` provider + extra headers; token-estimate calibration from real usage; `#[ignore]`d live smoke tests. Also isolated agent tests from the repo's own `CLAUDE.md`. 265 → 296 tests. Next: P3.1.
- **2026-09-28 — P1 complete.** Content-block `Message`, `ProviderEvent` stream + `ResponseAccumulator`, single-method `ModelProvider::stream` with shared cancellation, `ToolContext` cancellation, turn schema v2 (v1 still readable), `EventEnvelope` with `seq`, extended error taxonomy. Found and fixed along the way: the `execute` tool leaked the real command as an orphan on timeout (only the shell was killed) — now kills the process tree. 223 → 265 tests. Uncommitted. Next: P2.1 (retry/backoff), then P2.4 (Anthropic).
- **2026-09-28 — P0 complete.** Session-scoped approvals, Ollama tool calling, derived context budget + configurable tool-round cap, no more loop-transition panics, `RuntimeError` actually published, docs refreshed. 200 → 223 tests; fmt/clippy clean. Uncommitted. Next: P1.1 (content-block `Message`).

### Dependency graph

```
P0 ──────────────────────────────────────────────┐
P1 ──► P2 ──► P3 ──┬──► P4 ──► P6 ──┐            ├──► P7 (release)
                   └──► P5 ─────────┴────────────┘
```

P4 and P5 can run in parallel once P3 is done. P7's test-infrastructure tasks (P7.1 to P7.3) should start alongside P2, not wait until the end.

---

## Guiding decisions

These are settled for v2 unless a phase explicitly revisits them.

1. **No rewrite.** Crate layout stays. New code lands in the existing crates; no new crates unless a task below says so.
2. **Break the core contract once, cleanly.** P1 changes `Message`, `ModelProvider` and `Turn` in one coordinated pass rather than shimming. The persisted-data format gets a version field, and old v1 sessions stay readable (see P1.6).
3. **One event stream per inference.** Providers expose a single streaming call yielding typed events. Non-streaming inference is a helper that collects the stream, not a second trait method.
4. **The full turn trace is state, not decoration.** Tool calls and tool results are persisted and replayed into context. The v1 scope cut ("only the final answer survives") is reversed.
5. **Cancellation is cooperative and pervasive.** A `CancellationToken` is threaded through inference, tool execution and hooks.
6. **Profiles are data.** An agent's tool set, prompt, policy, model and memory strategy come from config. Coding is one profile, not the architecture.
7. **Every model-offered tool is described from one source.** Tool specs come from the registry (builtin, MCP, or user-registered), never from a parallel hand-maintained list.

---

## P0 — Housekeeping & quick correctness fixes

Small fixes that are wrong today and don't depend on the redesign.

- [x] **P0.1 Refresh stale docs.** `docs/v1-status.md` still says model-initiated tool calls aren't parsed (`run_tool_loop` does this). `CLAUDE.md` says 191 tests, but there are 200. Freeze v1-status as a historical record and point to this plan.
  - *Done:* `v1-status.md` carries a "frozen" banner naming its outdated items; `CLAUDE.md` points to this plan as the active source of truth, and its Ollama/tool-round/test-count statements are corrected.
- [x] **P0.2 Make "approve/deny for session" real.** Today `ApprovedForSession`/`AlwaysDeniedForSession` behave exactly like the once-variants. Add session-scoped allow/deny sets to `ApprovalContext` (`crates/arbe-tools/src/lib.rs`); `execute_gated` records session decisions; `StandardApprovalPolicy` consults them before its mode logic. Session denies take precedence over allows. A `High`-risk tool can be session-approved only if explicitly allowed by config (default: no — still prompts).
  - *Done:* new `arbe_tools::SessionApprovals` (shared `Arc<Mutex>` memory) on `ApprovalContext::session`, plus `ApprovalContext::new` and `session_approval_covers_high_risk` (`RuntimeConfig` field, default `false`). Session approval only upgrades `RequiresPrompt`; it never overrides a config-level deny or `DryRunOnly`. The TUI modal now says when `[a]` won't cover a high-risk tool. 11 new tests across `session_approvals.rs`, `policy.rs`, `gate.rs`.
- [x] **P0.3 Enable Ollama tool calling.** Ollama's `/api/chat` supports `tools`. Implement request/response mapping in `crates/arbe-providers/src/ollama.rs` and set `tool_calls: true`. (It will be re-ported in P2; doing it now lets the existing loop be exercised locally without an OpenAI key.)
  - *Done:* tools offered in Ollama's function format; tool calls parsed (inline-object or string-encoded arguments) with synthesized process-unique ids; tool results sent with `tool_name` recovered from the requesting call. The adapter previously sent no `temperature`/`max_tokens` at all — now sent as `options` together with an explicit `num_ctx` matching `capabilities()`. Models without tool support (Ollama's 400 "does not support tools") are retried without tools and remembered per model. The default model changed from `llama3` (no tool support) to `llama3.1`. 9 new tests. **Not verified live:** no Ollama server available in this environment.
- [x] **P0.4 Raise unrealistic defaults.** `MAX_TOOL_ROUNDS` 8 → configurable, default 50. `context_budget_tokens` 8 000 → default derived from `ProviderCapabilities::max_context_tokens` minus `max_tokens`.
  - *Done:* `RuntimeConfig::max_tool_rounds` (`ARBE_MAX_TOOL_ROUNDS`, default 50); `context_budget_tokens` is now `Option<u64>` (`ARBE_CONTEXT_BUDGET`), and `effective_context_budget(window)` derives it (falling back to half the window if `max_tokens` would consume all of it). OpenAI now gets ~124k instead of 8k. 3 new tests.
- [x] **P0.5 Replace `LoopMachine` panics.** Illegal transitions currently `.expect()`-panic inside `submit_message`. Return a `HarnessError::Internal` and emit `RuntimeError` instead, keeping a `debug_assert!` for development.
  - *Done:* `agent::advance()` replaces all 16 `.expect()` sites; `HarnessError::Internal` added (pulled forward from P1.5). Separately, the runtime never published `RuntimeError` at all — `submit_message` now publishes it, carrying the turn id, for *any* failed turn. 1 new test.
- [x] **P0.6 Commit or drop the pending `arbe-tui/src/lib.rs` change** so P1 starts from a clean tree.
  - *Done:* kept. It's a real fix (the header's `~tokens` now updates from each `ContextBuilt` event). Nothing is committed yet; P0 is ready to commit when you want.

**Exit criteria:** session approvals are covered by gate + policy tests; an Ollama model can call a builtin tool end-to-end in the existing loop (verified by a real run if an Ollama server is available, otherwise by unit tests on the mapping); `cargo fmt`, `clippy -D warnings`, `test` are clean.

---

## P1 — Core types v2

Redesign the shared contract in `arbe-core` (plus the provider trait in `arbe-providers`) that everything else builds on.

- [x] **P1.1 Content-block `Message`.** Replace `content: String` + `tool_calls`/`tool_call_id` side fields with typed blocks:

  ```rust
  pub struct Message {
      pub role: Role,                 // User | Assistant | System | Tool
      pub content: Vec<ContentBlock>,
      pub timestamp: DateTime<Utc>,
  }

  pub enum ContentBlock {
      Text { text: String },
      Image { source: ImageSource, media_type: String },
      ToolUse { id: String, name: String, input: Value },
      ToolResult { tool_use_id: String, content: Vec<ContentBlock>, is_error: bool },
      Thinking { text: String, signature: Option<String> },
      // provider-opaque blocks round-tripped verbatim (e.g. redacted thinking)
      Opaque { provider: String, data: Value },
  }
  ```

  Add a `CacheHint` marker (on a block or a message) that providers supporting prompt caching translate. Keep convenience constructors (`Message::user_text`, `Message::tool_result`, ...) so call sites stay readable.
  - *Done:* `arbe_core::{ContentBlock, ImageSource}`; `Message { role, content: Vec<ContentBlock>, timestamp, cache_breakpoint }` with `new`/`with_blocks`/`assistant_tool_calls`/`tool_result`/`tool_result_blocks`/`text()`/`tool_uses()`. The cache hint is a message-level `cache_breakpoint` flag. v1's string `content` still deserializes. Token estimation is now per block (`arbe_memory::estimate_message_tokens`, flat 1 600 tokens per image). Both adapters map images (OpenAI: content parts with data URLs; Ollama: base64 `images`).
- [x] **P1.2 Provider event stream.** Define the single streaming output type:

  ```rust
  pub enum ProviderEvent {
      TextDelta(String),
      ThinkingDelta(String),
      ToolUseStart { id: String, name: String },
      ToolUseInputDelta { id: String, partial_json: String },
      ToolUseEnd { id: String },
      Usage(Usage),
      Stop(StopReason),   // EndTurn | ToolUse | MaxTokens | StopSequence | Refusal | Other(String)
  }
  pub struct Usage { input_tokens: u64, output_tokens: u64, cache_read_tokens: u64, cache_write_tokens: u64 }
  ```

  Plus a pure `ResponseAccumulator` that folds events into a final assistant `Message` + `Usage` + `StopReason`. It must reassemble fragmented tool-use JSON, which is the thing v1 deliberately avoided.
  - *Done:* `arbe_providers::{ProviderEvent, ResponseAccumulator}`; `Usage`/`StopReason` live in `arbe-core` (shared with `Turn`/events). Added `ThinkingSignature` and `Opaque` events for Anthropic. Usage events carry cumulative counts, merged per field by max. Covered by a fragmentation property test (every chunk size of a unicode/escaped JSON payload).
- [x] **P1.3 New `ModelProvider` trait.**

  ```rust
  #[async_trait]
  pub trait ModelProvider: Send + Sync {
      fn id(&self) -> &str;
      fn capabilities(&self, model: &str) -> ModelCapabilities;
      async fn stream(&self, req: ModelRequest, cancel: CancellationToken)
          -> Result<BoxStream<'static, Result<ProviderEvent, ProviderError>>, ProviderError>;
  }
  ```

  `ModelCapabilities` is per-model (context window, max output, tools, vision, thinking, caching). `ModelRequest` gains `system: Vec<ContentBlock>` (separate from messages, since Anthropic requires it), `tools`, `tool_choice`, `thinking`, `stop_sequences`, and a `metadata` passthrough. A free function `infer(provider, req, cancel)` collects the stream through `ResponseAccumulator` for callers that don't need deltas.
  - *Done, with one deviation:* `ModelRequest` keeps system instructions as `Role::System` messages instead of a separate `system` field — adapters that need them separate (Anthropic) hoist them. This avoids churn in the context pipeline, which interleaves memory notes as system messages. `thinking`/`stop_sequences` fields are deferred to P2.4, when a provider uses them. Shared plumbing in `arbe_providers::http` (`send` with cancellation + status/`Retry-After` mapping, `text_chunks`, `cancellable`). Both adapters were ported (pulled forward from P2.2/P2.3), including OpenAI streamed tool-call reassembly, `stream_options.include_usage`, `reasoning_content` → thinking, and Ollama `thinking`/usage/`done_reason`.
- [x] **P1.4 Cancellation plumbing.** Add `tokio-util` (workspace dep) for `CancellationToken`. `ToolExecutor::execute` gains a `ToolContext { cancel, session_id, turn_id, progress: Option<ProgressSink> }` argument. `Hook::run` gets the token too.
  - *Done except hooks:* `ToolContext { cancel, progress }` (`ToolContext::report`), threaded through `execute_gated`. `execute` honours cancellation and now kills the **whole process tree** on cancel *or* timeout (process group + `kill -KILL -pgid` on Unix, `taskkill /T` on Windows). Previously only the shell died and the real command kept running as an orphan. The MCP bridge deliberately ignores cancellation until the P4.5 client rewrite (mid-request abandonment would desync its stdio stream). `Hook::run` does not take the token yet (hooks already time out; moved to P3.8). The Unix branch is verified against tokio's API but only compiled on Windows so far — CI will be the first Unix build.
- [x] **P1.5 Error taxonomy update.** `ProviderError` gains `Overloaded`, `ContextLengthExceeded`, `Cancelled`, and a `retry_after: Option<Duration>` on rate limits. Add `HarnessError::Cancelled` and `HarnessError::Internal`. Implement the `UserFacing` trait (defined in v1, never implemented) for every variant.
  - *Done:* plus `ProviderError::is_retryable()`, `ToolError::Cancelled`, and HTTP mapping for 503/529 (overloaded), context-length phrasings from OpenAI/Anthropic, and `Retry-After` seconds.
- [x] **P1.6 Persistence schema v2.** `Turn` becomes an ordered list of `Message`s (user, assistant, tool results, assistant...) plus per-turn `Usage` and `StopReason`, with `schema_version: 2`. `SessionStore` reads v1 lines by converting them (user + final assistant text only) so old sessions still resume. `SessionMeta` gains cumulative `Usage`, `title`, and `profile` settings snapshot.
  - *Done except the profile snapshot* (moved to P4.2, where profiles exist). `Turn::user_message()`/`final_assistant_message()` accessors. The agent now records per-turn usage/stop reason and keeps `meta.json`'s cumulative usage current. It still persists only [user, final answer] per turn — the full trace is P3.3. Storage test proves a v1 `turns.jsonl` loads and accepts v2 appends.
- [x] **P1.7 Event contract v2.** `RuntimeEvent` gains `ThinkingDelta`, `ToolUseInputDelta` (so UIs can show arguments forming), `ToolProgress`, `UsageUpdated`, `TurnCancelled`, `CompactionPerformed`. `RuntimeCommand` gains `CancelTurn`. Events carry a monotonic `seq` so a consumer can detect drops without relying on broadcast `Lagged`.
  - *Done:* `seq` lives on an `EventEnvelope` wrapper rather than every variant; `ToolUseInputDelta` is keyed by the provider's call id (the harness `ToolCallId` doesn't exist until the call is complete), and there's a matching `ToolUseStarted`. Emitted today: `ThinkingDelta`, `ToolUseStarted`, `ToolUseInputDelta`, `UsageUpdated`. `ToolProgress`/`CompactionPerformed`/`TurnCancelled`/`CancelTurn` are defined but not emitted/handled until P3.5/P5.2.

**Exit criteria:** all v1 crates compile against the new types (behavior may temporarily be stubbed behind P2/P3); `ResponseAccumulator` is property-tested for arbitrary fragmentation of tool-use JSON; a v1 `turns.jsonl` fixture loads through the v2 reader.

*Status:* all met — nothing was stubbed; every crate runs on the new types with real behavior.

---

## P2 — Provider layer v2

Port adapters to the new trait, add Anthropic, and make the network layer production-grade. Existing pure helpers (`SseDecoder`, `Utf8ChunkBuffer`, `error_map`) are kept.

- [x] **P2.1 Shared HTTP layer.** One `reqwest::Client` per provider with sensible connect/read timeouts; a `RetryPolicy` (exponential backoff + jitter, honors `retry_after`, retries on 429/5xx/overloaded/transport errors, never on 4xx validation) applied before the first byte of a stream is yielded; mid-stream failures surface as errors, not silent retries.
  - *Done:* `http::client()` (15 s connect, 5 min idle-read — no total timeout, since long answers are fine as long as they keep arriving). `arbe_providers::stream_with_retry` + `RetryPolicy` (4 retries, 1 s base doubling, 60 s cap, clock-based jitter; a `Retry-After` longer than the cap fails fast). Retrying happens only before the first event, including an error arriving *as* the first event. Waits are cancellable. Each retry is published as `RuntimeEvent::ProviderRetrying` and shown in the TUI activity line. `ARBE_MAX_RETRIES` configures it. New `ProviderError::Unreachable` (connection refused — *not* retried, with a "is the server running?" hint) vs `Network` (dropped mid-request — retried).
- [x] **P2.2 OpenAI adapter (Chat Completions) port.** Streaming tool calls via `ToolUseStart`/`ToolUseInputDelta`, `stream_options.include_usage` for usage, reasoning-model parameter quirks kept.
  - *Done in P1.3:* implemented and fixture-tested. **Live run pending** (`openai_live` in `tests/live.rs`).
- [x] **P2.3 Ollama adapter port.** Carry P0.3's tool mapping across; usage from `prompt_eval_count`/`eval_count`.
  - *Done in P1.3:* implemented and fixture-tested; the "does not support tools" fallback works on the streaming path; `num_ctx` now comes from the catalog. **Live run pending** (`ollama_live`).
- [x] **P2.4 Anthropic adapter (Messages API).** Native content blocks, streaming (`message_start`/`content_block_delta`/...), tool use, extended thinking with signature round-tripping, prompt caching via `cache_control` from `CacheHint`, image input.
  - *Done:* `AnthropicProvider` (`ANTHROPIC_API_KEY`, default model `claude-sonnet-5`). System messages are hoisted into `system`; consecutive same-role messages are merged, with tool results moved first; an assistant-first history gets a placeholder user turn. Only *signed* thinking is echoed back; redacted thinking round-trips as `Opaque`. Non-object tool input is wrapped. Cache breakpoints go on the last system block, on flagged messages, and on the final message (conversation caching across tool rounds). `ModelRequest::thinking_budget_tokens` (`ARBE_THINKING_BUDGET`) is added on top of `max_tokens` and drops `temperature`. The stream translator is tested against the documented event sequence, including mid-stream `error` events. **Live run pending** (`anthropic_live`).
- [x] **P2.5 OpenAI-compatible profile.** Treat "OpenAI-compatible gateway" (vLLM, LM Studio, OpenRouter, Azure-style base URLs, custom headers) as configuration of the OpenAI adapter, not a new adapter.
  - *Done:* provider id `openai_compatible` (`OpenAiProvider::compatible`) — `base_url` required, key optional, catalog lookups fall back to OpenAI model names. `with_headers` on OpenAI/Anthropic; `ARBE_HTTP_HEADERS="Name: value; ..."`; `ARBE_API_KEY` is a generic key fallback. Azure's deployment-path/`api-key` scheme is not covered.
- [x] **P2.6 Model catalog.** A small built-in table of known models → `ModelCapabilities` (context window, max output, tool/vision/thinking support), overridable from config for unknown or custom models.
  - *Done:* `ModelCatalog` — longest-prefix match (dated snapshots resolve), per-provider defaults, and exact-name overrides via `with_override`. Values are the published limits as of 2026-09 (e.g. gpt-5 400k, gpt-4.1 ~1M, Claude 200k). No `max_output_tokens` yet. Reading overrides from config is part of P4.1.
- [x] **P2.7 Token counting.** Keep `estimate_tokens` as the fallback; prefer real `Usage` from the last response to calibrate (track actual input tokens per turn and use the delta for the next estimate). Optional provider-native count endpoint behind the trait where available.
  - *Done (calibration part):* `arbe_memory::TokenCalibration` — an EMA of reported/estimated input tokens, clamped to [0.5, 3.0] per observation. It is learned from each turn's first request, applied to the context budget and to the displayed estimate, and it absorbs tool-definition and formatting overhead too. The provider-native count endpoint is not done (not needed while calibration holds up).
- [x] **P2.8 Provider registry.** Replace the `build_provider` match with a registry keyed by provider id, so adding a provider doesn't require editing a central match and third parties can register their own.
  - *Done:* `ProviderRegistry` (`with_builtins`, `register`, `build`, `ids`) + `ProviderSettings { api_key, base_url, extra_headers, catalog }`. `build_provider` remains as shorthand. The agent builds through the registry. Injecting a custom registry into `Agent` comes with the library facade (P6.1).
- [x] **P2.9 Recorded-fixture tests.** For each adapter: request-body golden tests, and replayed recorded streams (SSE / NDJSON fixture files) through the decoder → `ProviderEvent`s → `ResponseAccumulator`. No network.
  - *Done, with a caveat:* every adapter has request-body tests and full stream-translation tests folded through the accumulator. The stream payloads are inline and **hand-written from the documented wire formats, not captured from real traffic**. Replace them with real captures once the live tests have been run.

**Exit criteria:** the same agent turn (with a tool call) runs on OpenAI, Anthropic and Ollama by config switch only; all three pass fixture tests; a live smoke test (P7.2) passes for every provider whose credentials are available, with results recorded in the status table.

---

## P3 — Agent loop v2

Replace the 1 358-line `Agent` with a small, cancellable loop that persists everything it does. Lives in `arbe-runtime`; split into modules (`session.rs`, `turn.rs`, `tool_exec.rs`, `approvals.rs`) rather than one file.

- [x] **P3.1 Split `Agent` into `Session` + `TurnRunner`.** `Session` owns state (store, history, meta, registry, policy, config snapshot). `TurnRunner` executes one turn against a `&Session` snapshot and returns the new messages to commit. This removes "hold `&mut Agent` for the whole turn" and with it the need for the `ToolDecisions` lock workaround (keep the mailbox concept, drop the lock-contention reason).
  - *Done, differently named:* the public type stays `Agent` (it *is* the session handle; renaming would only churn callers), with state behind a short-lived internal mutex and every method taking `&self`. `turn::TurnRunner` runs one turn. The outer `tokio::Mutex` in the TUI and the public `ToolDecisions` handle are gone; `supply_tool_decision`/`cancel_turn` work mid-turn. A concurrent `submit_message` returns `HarnessError::Busy`.
- [x] **P3.2 Unified streaming loop.** Every round streams via `ModelProvider::stream`, forwarding `TextDelta`/`ThinkingDelta`/`ToolUseInputDelta` as events. No separate direct-response vs. tool-loop branches. Loop ends on `StopReason::EndTurn`, round limit, cancellation or error.
  - *Done:* `TurnRunner::model_loop`/`stream_inference`. Cancellation mid-stream returns the partial response (kept, minus incomplete tool calls) instead of an error.
- [x] **P3.3 Persist and replay the full trace.** Assistant tool-use messages and tool-result messages are appended to history and `turns.jsonl` as they happen (not only at turn end), so a crash mid-turn loses at most the in-flight tool call. Resume replays them into context.
  - *Done:* `in_flight.jsonl` write-ahead log (`SessionStore::{append,read,clear}_in_flight`), full trace in `Turn.messages` on commit, `recover_interrupted_turn` on resume (idempotent). History trimming is now turn-granular (`arbe-memory::truncation::select_kept`), condensing a too-big turn to question + final answer, so a tool call and its result are never separated.
- [x] **P3.4 Parallel tool execution.** Tool calls from one assistant message are approved in order (one modal at a time), then executed concurrently (bounded `JoinSet`), with results committed in the model's original order. Tools can declare `parallel_safe: false` (e.g. `execute`, `write_file` on the same path) to force sequential execution.
  - *Done:* `ToolExecutor::parallel_safe()` (default `true`; `false` for `write_file`, `edit_file`, `execute`, `todo_write`). Consecutive parallel-safe calls run via `join_all`; an unsafe call runs alone, in order. Tested by overlapping run intervals, not timing thresholds. Required splitting the gate: `authorize` returns an `Authorized` that only the gate can construct, so execution can't be reached without approval.
- [x] **P3.5 Cancellation.** `CancelTurn` cancels the turn's token: in-flight inference stream is dropped, running tools get the token (`execute` kills its child process), pending approvals resolve as denied. The partial assistant message is persisted with `StopReason::Cancelled`; history stays valid (every `ToolUse` gets a matching synthetic `ToolResult { is_error: true, "cancelled" }`).
  - *Done:* `Agent::cancel_turn()`; covered for mid-stream, pending approval (the prompt is withdrawn) and running tools. Tools that already ran keep their real results. `TurnCancelled` event; `submit_message` returns `HarnessError::Cancelled`. `RuntimeCommand::CancelTurn` has no dispatcher yet (commands arrive with headless mode, P6.2).
- [x] **P3.6 Loop guards.** Configurable max rounds, per-turn token/cost ceiling, repeated-identical-tool-call detection. Hitting a guard ends the turn with an explicit stop reason and event, not a canned apology string.
  - *Done:* `StopReason::{ToolRoundLimit, TurnTokenLimit, RepeatedToolCall}` (+ `Interrupted`), carried on `TurnCompleted`; `max_turn_tokens` (`ARBE_MAX_TURN_TOKENS`); 3 identical rounds = stuck. The TUI shows a notice. Also changed: an all-denied round no longer ends the turn with "I don't have permission" — the model sees the denial and decides what to say.
- [ ] **P3.7 Tool result handling.** Structured results (`Vec<ContentBlock>`, so tools can return images); per-tool output size caps with truncation markers; `is_error` results flow back to the model rather than aborting the turn (only harness-level failures abort).
  - *Mostly done:* errors go back as `is_error` results; output is capped head+tail (`max_tool_output_chars`, `ARBE_MAX_TOOL_OUTPUT_CHARS`). **Remaining:** `ToolResult` still carries JSON only, so a tool can't return an image — needs a blocks field on `arbe_core::ToolResult`.
- [x] **P3.8 Hook phases fully wired.** Call all seven `HookPhase`s at their points (`BeforeContextAssembly`, `BeforeToolExecute` — which may veto or rewrite arguments, still inside the gate — `AfterToolExecute`, `OnError`) with typed payloads rather than ad-hoc `json!`.
  - *Done:* all seven phases fire; payload structs in `agent/hooks.rs`. `BeforeToolExecute` runs before the gate (so the human approves the final arguments) and can rewrite `arguments` or set `veto`. `Hook::run` does not get the cancellation token — hooks already have a hard timeout, so it wasn't worth another trait change.
- [x] **P3.9 Remove the manual `/tool` bypass path's duplication.** `propose_tool_call`/`resolve_tool_call` become a thin "inject a synthetic tool-use" entry point into the same `TurnRunner` path, so there is exactly one execution path to test.
  - *Done:* `Agent::invoke_tool` runs a one-call round through `tools::run_round`; `propose_tool_call`/`resolve_tool_call`/`pending_tool_calls` are gone.
- [ ] **P3.10 TUI port.** Update `arbe-tui` for the v2 events: thinking display (collapsible), live tool-argument rendering, parallel tool status, `Esc` to cancel a turn, usage/cost in the header, and the existing session picker wired to `resume`.
  - *Mostly done:* `Arc<Agent>`, `Esc` cancels (also from the approval modal), stop-reason notices, session tokens in the header, retry/thinking/tool-prep/progress in the activity line; resume shows tool-call counts. **Remaining:** a collapsible thinking view and live tool-argument rendering (both only surface in the activity line today), cost (needs prices in the catalog), and a human run at a real terminal.

**Exit criteria:** `Agent`-level tests (with a scripted fake provider) cover: multi-round tool use, parallel tools with ordering, cancel mid-stream and mid-tool, crash-then-resume mid-turn with trace intact, every loop guard; `agent.rs`-equivalent modules have no function over ~150 lines; interactive TUI run verified by a human at a real terminal.

---

## P4 — Config, profiles & extension wiring

Make the harness configurable from files and connect the parts v1 built but never wired up.

- [x] **P4.1 TOML config.** Load `~/.arbe/config.toml` plus `<project>/.arbe/config.toml` (project overrides global), then env vars, then CLI flags. Schema follows the `[profile]`/`[memory]`/`[tools]`/`[hooks]`/`[mcp]` draft in `v1-implementation-plan.md`, extended for providers and profiles. Validation errors are `ConfigError`s with file/line and a likely fix. `RuntimeConfig::from_env` becomes one layer of this.
  - *Done:* `arbe_runtime::config` (`mod.rs` layering, `file.rs` schema). Files: `~/.arbe/config/config.toml` (the reserved `config/` dir) and `<workdir>/.arbe/config.toml`. Every table is `deny_unknown_fields`, so typos are errors with file + line; a plain `api_key` is refused and `provider.api_key_env` names the variable instead. `load_from(files, env_fn, project_dir)` takes the environment as a function, so tests never touch process env. Invalid `ARBE_*` numbers are now errors. `main` exits with code 2 and the message on a bad config. Sections: `provider`, `generation`, `context`, `loop`, `approval`, `hooks`, plus `tools`, `prompt`, `[profiles.*]`, `[[models]]`.
- [x] **P4.2 Profiles.** A profile = model + provider + system prompt template + tool allow-set + approval policy + memory strategy + skills + hooks. Ship built-in profiles `coding` (today's behavior) and `general` (no filesystem/execute tools, no project sandbox). `--profile <name>` selects one.
  - *Done:* built-in `coding` (all tools, coding prompt) and `general` (`todo_write` only, general prompt); `[profiles.<name>]` in either file overlays the base settings; `--profile`/`ARBE_PROFILE`/`profile = ...`. Unknown names fail at startup with the known list. The tool allow-set is enforced by construction (`ToolRegistry::retain`), so disallowed tools aren't callable even via `/tool`. Prompt templates: `system_prompt::PromptTemplate` (`coding`, `general`, or a file with optional placeholders, re-read every turn). **Not done:** the profile settings snapshot in `SessionMeta` (P1.6 leftover); a resumed session uses the current profile.
- [x] **P4.3 Registry-derived tool specs.** `ToolExecutor` gains `fn spec(&self) -> ToolSpec` and `fn default_risk(&self) -> RiskLevel`. Builtin specs are generated from each tool's `Args` via `schemars` (new workspace dep), eliminating the hand-maintained `tool_specs()` list. The model is offered exactly the registry's tools filtered by the profile's allow-set.
  - *Done, slightly different shape:* `ToolExecutor::description() -> ToolDescription` (the registry supplies the name) and `default_risk()`. `ToolDescription::from_args::<Args>(text)` derives the schema with subschemas inlined, and simplifies it for models/strict APIs (drops Rust-only `format`s and `[T, "null"]` types). The hand-written `tool_specs()`/`default_risk_for()` are gone; `ToolRegistry::specs()`/`risk_of()` replace them, so tools registered at runtime are offered to the model too.
- [ ] **P4.4 MCP wiring.** At session start, spawn enabled servers from `servers.toml`, `initialize`, `tools/list` (now keeping `inputSchema` — `McpToolInfo` currently drops it), and register into the same registry. MCP tool names are sanitized for provider name rules (`server__tool`, since `/` is rejected by several APIs). Failed servers are reported via an event, not fatal. Add HTTP (streamable) transport alongside stdio.
- [ ] **P4.5 MCP client robustness.** Concurrent requests (a reader task + id → oneshot map instead of one `Mutex<McpClient>` serializing all calls), server stderr captured to `~/.arbe/logs/mcp/<server>.log`, `tools/list_changed` notifications refresh the registry, reconnect on crash.
- [ ] **P4.6 Skill scopes.** Load global, project (`<project>/.arbe/skills/`) and session scopes through `merge_skills`. Add on-demand skill loading: only skill names + descriptions go in the prompt; a `load_skill` tool pulls the body into context, instead of always injecting every skill's full text.
- [ ] **P4.7 Command hooks.** Config-declared hooks that run a shell command with the JSON payload on stdin and read a (possibly transformed) payload or a veto decision from stdout, with the existing timeout/isolation guarantees. Rust `Hook` impls remain for embedders.
- [ ] **P4.8 Permission rules.** Extend the allow/deny lists from bare tool names to argument-aware patterns (e.g. `execute(cargo test*)`, `write_file(src/**)`), stored in config and in session approvals.
- [ ] **P4.9 Secrets handling.** API keys from env or an OS keychain reference in config (never plain text in `config.toml`), and redaction of known secret values from events, logs and persisted tool outputs.

**Exit criteria:** switching between the `coding` and `general` profiles changes tools/prompt/policy with no code change (test-verified); a real MCP server's tools are callable by the model (verified against the in-tree reference server from P7.3); a command hook can veto a tool call.

---

## P5 — Context management v2

- [ ] **P5.1 Async `ContextStrategy`.** Make `build_context` async and give it access to a provider handle, so strategies can call a model.
- [ ] **P5.2 LLM compaction.** Replace the placeholder `CompactWithSummaryStrategy` with real summarization: when history exceeds a threshold (e.g. 80% of window), summarize the oldest span into a persisted `Compaction` record (so it's done once, not every turn), keep tool-use/tool-result pairs intact at the cut point, emit `CompactionPerformed`. Manual `/compact` command in the TUI.
- [ ] **P5.3 Tool-output pruning.** Older, large tool results are replaced with short stubs ("[read_file output, 12 KB, elided]") before dropping whole messages — cheaper and less lossy than summarizing.
- [ ] **P5.4 Prompt-cache-aware assembly.** Stable prefix ordering (system → tools → skills → history) with `CacheHint`s at the boundaries so providers that support caching get hits across turns.
- [ ] **P5.5 Nested project instructions.** Discover `agent.md`/`AGENTS.md`/`CLAUDE.md` in subdirectories and inject a directory's instructions when the agent first touches a file under it (from `docs/todo.md`).
- [ ] **P5.6 Persistent memory tool.** A `memory` tool the model can use to read/append to the global and project `memory.md` files (currently read-only inputs), gated like any other write.

**Exit criteria:** a scripted 200-turn session with large tool outputs stays within budget without ever breaking tool-use/result pairing (test-verified); compaction is persisted and survives resume.

---

## P6 — Multi-purpose & embedding

- [ ] **P6.1 Library facade.** A documented, stable-ish public API in `arbe-runtime` (`Harness::builder()...build()`, `session.send(...)` returning an event stream, `session.cancel()`), with an `examples/` directory showing a minimal embedder.
- [ ] **P6.2 Headless mode.** `arbeharness --headless` speaks JSON-RPC over stdio (`RuntimeCommand` in, `RuntimeEvent` out), so editors, scripts and other UIs can drive the harness. Also a one-shot `arbeharness -p "<prompt>"` mode that prints the final answer (and optional JSON event log) and exits with a meaningful status code.
- [ ] **P6.3 Subagents.** A `task` tool that spawns a child `Session` with its own profile, fresh context and a restricted tool set, and returns its final answer as the tool result. Child events are forwarded with a parent id so UIs can nest them; child approvals route to the same human. Depth and concurrency are capped.
- [ ] **P6.4 Background execution.** `execute` gains `background: true`, returning a handle; `process_output` / `process_kill` tools poll and stop it. Background processes are tied to the session and killed on close.
- [ ] **P6.5 `ask_user` tool.** A structured clarifying question (with options) that pauses the turn like an approval does and resumes with the answer.
- [ ] **P6.6 Plan mode.** A read-only mode (write/execute tools auto-denied) with `exit_plan_mode` requiring user approval of the plan — implemented as a profile/policy overlay, not a new `LoopPhase`.
- [ ] **P6.7 Web tools (optional, `general` profile).** `web_fetch` (HTML → text, size-capped) and a pluggable `web_search` backend configured in config; medium risk.
- [ ] **P6.8 Observability.** `tracing` spans per turn/round/tool with a file sink under `~/.arbe/logs/`, optional OpenTelemetry export behind a feature flag, and per-session usage/cost summaries.

**Exit criteria:** the headless mode is driven end-to-end by an integration test (spawns the binary, sends a message, reads events); a subagent test shows context isolation and approval routing; the minimal embedder example compiles in CI.

---

## P7 — Verification, hardening & release

Start P7.1 to P7.3 alongside P2; they are infrastructure the other phases need.

- [ ] **P7.1 HTTP mock tests.** Add `wiremock` (dev-dep) and cover `stream` end-to-end per adapter: retries, rate limits with `retry_after`, mid-stream disconnects, malformed chunks.
- [ ] **P7.2 Live smoke tests.** `#[ignore]`d tests (run via `cargo test -- --ignored`) that hit real OpenAI / Anthropic / Ollama when their env vars are present; a manual CI workflow (`workflow_dispatch`) with secrets. Record results in this plan's status table.
  - *Partly done (with P2):* `crates/arbe-providers/tests/live.rs` — a text round trip plus a tool round trip (incl. echoing the assistant message back) per provider, each skipped without credentials. Remaining: the `workflow_dispatch` CI job, and actually running them.
- [ ] **P7.3 Reference MCP server.** A tiny stdio MCP server in `tests/fixtures/` (a small Rust test binary) so `McpClient` and the P4.4 wiring get real process-level integration tests — closing v1's untested-client gap.
- [ ] **P7.4 Gate enforcement.** Make `ToolExecutor::execute` unreachable except via the gate: e.g. executors receive a `GateToken` that only `execute_gated` can construct. Closes the open item from the 2026-08-04 review.
- [ ] **P7.5 JSONL read performance.** Fix the deferred `list_turns`/`list_events` full re-read (append-aware index or in-memory tail cache in `SessionStore`).
- [ ] **P7.6 Benchmarks.** `criterion` benches for context assembly over large histories and stream-decode throughput, plus a first-token-latency measurement against the mock server. Only now, when there's a representative load profile.
- [ ] **P7.7 CI actually running.** Push to the remote and get the fmt/clippy/test matrix green on GitHub Actions for all three OSes; add `cargo-deny` (licenses/advisories).
- [ ] **P7.8 Docs & release.** `docs/v2-status.md` (honest acceptance checklist in the style of v1-status), user-facing README (install, config reference, profiles, MCP setup), `CHANGELOG`, tagged `v0.2.0` release with prebuilt binaries for the three platforms.

**Exit criteria:** all of the above green; `v2-status.md` has no item marked done that isn't verified.

---

## Keep / change / new — at a glance

| Area | Disposition | Where |
|---|---|---|
| Crate layout, dependency rules | Keep | workspace |
| `SessionStore`, atomic writes, JSONL recovery | Keep, extend schema (P1.6) | `arbe-storage` |
| `path_guard`, builtin tool implementations | Keep, adapt to `ToolContext` + schemars specs | `arbe-tools/builtin` |
| `execute_gated`, `StandardApprovalPolicy` | Keep, add session scope + patterns (P0.2, P4.8) | `arbe-tools` |
| `SseDecoder`, `Utf8ChunkBuffer`, error mapping | Keep | `arbe-providers` |
| MCP protocol framing | Keep; client rewritten for concurrency (P4.5) | `arbe-mcp` |
| Hook isolation (`HookRegistry`) | Keep, add command hooks (P4.7) | `arbe-hooks` |
| Skills parsing / merge | Keep, add scopes + on-demand loading (P4.6) | `arbe-skills` |
| `Message`, `Turn`, `RuntimeEvent` | **Redesign** (P1) | `arbe-core` |
| `ModelProvider` trait, adapters | **Redesign** (P1.3, P2) | `arbe-providers` |
| `Agent` | **Replace** with `Session` + `TurnRunner` (P3) | `arbe-runtime` |
| `ContextStrategy` | **Make async**, real compaction (P5) | `arbe-memory` |
| TUI | Keep structure, port to v2 events (P3.10) | `arbe-tui` |

---

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| P1 breaks every crate at once and the tree stays red for long | Stalled progress, hard review | Do P1 on a branch; land types + accumulator first with the old `Agent` adapted minimally, then swap in P3. Keep `cargo test` green at every commit. |
| Provider APIs drift (new parameters, deprecations) | Adapter breakage | Recorded fixtures (P2.9) + live smoke tests (P7.2) + model catalog overrides in config (P2.6). |
| Parallel tool execution introduces races on the filesystem | Corrupted edits | `parallel_safe` flag (P3.4); conservative defaults — writes and `execute` sequential. |
| LLM compaction loses critical details | Degraded long sessions | Persist compactions, keep tool pairs intact, keep pinned turns verbatim, make threshold configurable, allow `/compact` preview. |
| Scope creep in P6 | v2 never ships | P6.6 and P6.7 are optional; the v2.0 release bar is P0 to P5 + P6.1 to P6.3 + P7. |
| No human verification of interactive TUI in this environment | UI regressions | Keep TUI logic unit-testable (app state separate from rendering); each phase lists a manual verification step for a human run. |

---

## Open questions

1. **OpenAI Responses API vs. Chat Completions.** Plan assumes Chat Completions (broadest gateway compatibility). Responses API gives better reasoning-model support; revisit after P2.4.
2. **Headless protocol shape.** JSON-RPC 2.0 vs. raw newline-delimited `RuntimeEvent`s — decide at P6.2; leaning JSON-RPC for request/response correlation.
3. **Sandboxing `execute`.** v2 keeps "always gated"; OS-level sandboxing (containers, Seatbelt, landlock, Windows job objects) is out of scope for v2 unless prioritized.
4. **Windows symlink testing.** Unix-only symlink escape tests remain; consider a privileged Windows CI job.
