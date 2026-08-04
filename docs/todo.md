# Feature gaps (from comparing our system prompt against a reference agent's)

Surfaced 2026-08-03 while updating `arbe-runtime::system_prompt`'s template
(see `docs/tmp/system-prompt.md` for the original design). Not a commitment
to build all of these — a punch list to pick from.

| Feature | What it does | Where it'd land |
|---|---|---|
| Nested project instruction files | Scans the whole repo tree for `AGENTS.md`/`AGENT.md`/`Claude.md` (not just root `agent.md`/`CLAUDE.md`), with deeper files taking precedence over shallower ones for the code they cover | `arbe-storage::instructions` — currently we only check `project_dir` root and don't walk subdirectories at all (child-directory discovery, the inverse of the parent-walk we explicitly deferred; needs per-file-touched scope resolution, not just prompt assembly) |
| ~~Todo/task tracking tool exposed to the model~~ | A `todo_write`-equivalent tool so the *model itself* can track and surface multi-step progress | **Done** — `arbe-tools::builtin::todo_write::TodoWriteTool`, see `CLAUDE.md` |
| Plan mode | A read-only exploration phase before ambiguous/high-impact work, with explicit enter/exit and user approval of the plan | Would need a new `LoopPhase` variant plus `enter_plan_mode`/`exit_plan_mode` tools — nothing like this exists in `arbe-core`'s phase graph today |
| `ask_user_question`-style tool | A structured way for the model to ask a narrow clarifying question mid-task | No such tool in `arbe-tools::builtin` |
| Background/async tool execution | Start a long-running command, poll it, kill it, while continuing other work | Our `execute` tool is synchronous-only (with a timeout) — no background task registry |
| Subagents | Spawning parallel sub-conversations to isolate context or parallelize independent work | Nothing like this — `Agent` is single-threaded per session |
| MCP tool schema lookup before first use | A `search_tool` step required before ever calling a newly-announced MCP tool | `arbe-mcp` exposes tools directly via `tools/list`; no enforced "fetch schema first" step in the wiring |
| Rich output formatting conventions | PR/issue refs as markdown links, `startLine:endLine:filepath` code block headers, absolute-path file links | This is TUI/prompt-level rendering guidance, not runtime — could go in the template's wording but isn't implemented/enforced anywhere |

# Code review findings (2026-08-04, fixes applied same day)

Full-codebase review against the "pristine" bar — correctness, security, performance, memory, idiomatic Rust. Every crate (`arbe-core`, `arbe-storage`, `arbe-memory`, `arbe-providers`, `arbe-hooks`, `arbe-mcp`, `arbe-skills`, `arbe-tools`, `arbe-runtime`, `arbe-tui`) was read in full and findings verified against the actual source, not guessed. Every item below except one Medium was fixed the same day, with regression tests added and the full workspace (`cargo fmt`, `cargo clippy -D warnings`, `cargo test --workspace` — 200 tests) verified clean afterward.

## Critical — both fixed

- ~~**Symlink sandbox escape in every builtin filesystem tool.**~~ **Fixed.** `path_guard::verify_no_symlink_escape` canonicalizes the resolved path's nearest existing ancestor and re-checks it against a canonicalized `root`; wired into `read_file`/`write_file`/`edit_file`/`list_dir`/`glob`/`grep` right before their I/O call. Unix-only regression tests (`rejects_a_symlink_inside_root_that_points_outside_it`, `rejects_a_symlinked_file_inside_root_pointing_outside_it`) added — Windows CI can't create symlinks unprivileged.
- ~~**Streaming providers corrupt multi-byte UTF-8 split across chunk boundaries.**~~ **Fixed.** New `arbe-providers::utf8_buffer::Utf8ChunkBuffer` buffers raw bytes across chunks and only decodes complete UTF-8 sequences; wired into both `openai.rs` and `ollama.rs`'s streaming loops. Regression tests cover 2-byte and 3-byte characters split at every possible byte boundary.

## High — all four fixed

- ~~**`append_line` not a single write syscall.**~~ **Fixed.** `atomic::append_line` now builds the line+newline into one buffer and issues a single `write_all`. `session_store::read_jsonl` now only fails on a malformed line that *isn't* the last one — a torn trailing line (the only kind a crash mid-append can produce) is dropped instead of failing the whole session load. Regression tests: `list_turns_drops_a_torn_trailing_line_but_keeps_earlier_ones`, `list_turns_still_errors_on_a_malformed_line_that_is_not_the_last`.
- ~~**MCP `request()` has no timeout.**~~ **Fixed.** Wrapped in `tokio::time::timeout` (30s) in `arbe-mcp/src/client.rs`.
- ~~**`build_system_prompt` blocking I/O on every turn.**~~ **Fixed.** New `agent::build_system_prompt_async` wraps the existing sync reader in `tokio::task::spawn_blocking`; used at the `submit_message` hot-path call site. The one-time call at session-assembly stays sync (not the hot path, and converting it would require making `Agent::new`/`resume` async, which risks reintroducing the next item's class of bug in the TUI's threading model).
- ~~**`/tool` command blocks the TUI render loop.**~~ **Fixed.** `submit_input`'s `/tool` branch now goes through `handle.spawn` instead of `handle.block_on`, matching every other agent call.

## Medium — six fixed, one deferred

- ~~**`write_atomic`'s temp file name isn't unique.**~~ **Fixed.** Temp path now includes a `uuid::Uuid::new_v4()` suffix.
- ~~**`StandardApprovalPolicy` ignores `ToolInvocation::risk`.**~~ **Fixed.** A `RiskLevel::High` call now always resolves to `RequiresPrompt` when the mode/allowlist/denylist would otherwise have auto-approved it (DryRunOnly's AutoDeny is left alone — already maximally safe). Three regression tests added.
- ~~**System prompt template substitution can cross-contaminate.**~~ **Fixed.** `render_system_prompt` now splits `TEMPLATE` around both placeholders once and assembles the pieces, instead of two sequential `.replace()` passes. Regression test with global instructions containing the literal `{project_instructions}` string.
- ~~**`edit_file` has no read-size cap.**~~ **Fixed.** Added the same 5 MB `MAX_READ_BYTES` guard `read_file` has.
- ~~**Timed-out hooks keep running in the background.**~~ **Fixed.** `registry::run_phase` now calls `.abort()` on the `JoinHandle` in the timeout branch. Regression test (`a_timed_out_hook_is_actually_aborted_not_just_ignored`) uses paused virtual time to prove the task doesn't run to completion after the abort.
- ~~**MCP `request()` assumes the next stdout line is always the matching response.**~~ **Fixed.** `request()` now loops, skipping lines with no `id` field (notifications), until a response arrives or it times out.
- ~~**`ModelStreamChunk` broadcast can silently drop tokens under backpressure.**~~ **Fixed.** `drain_runtime_events`'s `Lagged` arm now sets `app.status_message` with the number of skipped events instead of silently continuing. Two regression tests added.
- ~~**`Agent::submit_message` clones the entire session history every turn.**~~ **Fixed.** `ContextInput`/`ContextPipeline::assemble`/`ContextStrategy::build_context` now take borrowed slices (`&[HistoryEntry]`, `&[u64]`) instead of owned `Vec`s; only the messages actually kept after budget selection get cloned (unavoidable — they need to outlive the input slices to go into the provider request).
- ~~**Full transcript re-parsed as markdown every TUI render frame.**~~ **Fixed.** `App` now holds a `rendered_cache`/`rendered_cache_entry_count` pair: every transcript entry except the current last one (the only one that ever mutates in place, via streaming) is rendered once and cached; `transcript_lines` only re-renders newly-settled entries plus the current last one, every frame. Four regression tests added, including one proving streaming deltas into the last entry don't grow the cache.
- **`list_turns`/`list_events` re-read and re-parse the whole JSONL file on every call.** **Not fixed** — `crates/arbe-storage/src/session_store.rs:114,128`. Deferred: a correct fix needs an invalidation-safe in-memory cache (e.g. tracking file length/mtime or an append-aware line count) inside `SessionStore`, which is more state and more ways to get stale than the other fixes here justified fixing blind. Worth addressing if long-running sessions become a real target — flagging instead of rushing it.

## Low — all fixed except two purely-informational items

- ~~`glob_tool`'s truncate-during-walk-then-sort behavior~~ **Documented** (not changed — sorting only the truncated subset is correct given the memory-bound goal; changed a bare comment gap into an explicit doc comment on `walk_and_match` explaining the ordering caveat).
- ~~`todo_write` clones `args.todos` twice.~~ **Fixed** — now serializes to JSON once before moving the `Vec` into storage, instead of cloning for storage and serializing separately.
- ~~`dirs_home` panics with a raw `Option::expect` message.~~ **Fixed** — explicit message naming `HOME`/`USERPROFILE`.
- ~~`list_sessions` aborts the entire listing on one corrupt `meta.json`.~~ **Fixed** — skips unreadable/unparseable entries instead of propagating.
- ~~`paths.rs` test mutates the process-global `ARBE_HOME` env var.~~ **Fixed** — `arbe_home()` split into a pure `resolve_arbe_home(Option<String>, PathBuf) -> PathBuf` that's tested directly, no env var mutation at all.
- ~~OpenAI's `is_final` always `false`.~~ **Fixed** — an explicit `is_final: true` chunk is now yielded on `SseItem::Done`.
- `ollama.rs`/`sse.rs`'s per-line `.to_string()` before `drain` — **left as-is**; on inspection this isn't an avoidable extra allocation (an owned `String` is needed either way once the buffer is drained), so there's nothing to fix here without added complexity for no real gain.
- `arbe-hooks`'s per-hook payload clone — **left as-is**, documented as fine at current scale (a handful of hooks, not-huge payloads); flagged to revisit only if that changes.
- `arbe-skills::merge_skills`'s O(n²) dedup — **left as-is**, documented as fine at realistic (tens of) skill counts; flagged to revisit if skill counts scale into the hundreds.
- ~~`arbe-tui`'s `content_line_count` recomputes from scratch every frame.~~ **Fixed** — `App` now maintains `settled_content_lines` incrementally (same settled/last split as the markdown cache above), so it only rescans the current last entry per call instead of the whole transcript.

## Test coverage gaps — four closed, two remain open

- ~~No symlink-escape test for `path_guard`.~~ **Closed**, see Critical section.
- ~~No test for a truncated/corrupt trailing line in `turns.jsonl`/`events.jsonl`.~~ **Closed**, see High section.
- ~~`select_kept` untested at `budget_tokens == 0` / pinned-alone-exceeds-budget.~~ **Closed** — `select_kept_with_zero_budget_and_no_pinned_keeps_nothing`, `select_kept_keeps_all_pinned_turns_even_when_they_alone_exceed_the_budget` added to `truncation.rs`.
- ~~No test covers the TUI `Lagged` broadcast-drop path.~~ **Closed**, see Medium section.
- **Nothing structurally enforces `ToolExecutor::execute` is only reachable through `gate::execute_gated`.** Still open — this needs a lint/architecture-test (e.g. a `cargo-deny`-style check or a `#[deny]`-worthy visibility restructure), not a unit test; out of scope for this pass.
- **No test mocks the HTTP layer for `infer`/`infer_stream` in `arbe-providers`.** Still open — would need a mock-HTTP-server dependency (e.g. `wiremock`) that isn't in the workspace today; the pure chunk-decoding logic (`Utf8ChunkBuffer`, `SseDecoder`) that the original bug lived in is now unit-tested directly instead, which covers the specific regression risk without pulling in that dependency.
- **No test covers the TUI `/tool`-during-streaming lock contention path directly** (i.e. an end-to-end proof the render loop stays responsive) — the `handle.block_on` → `handle.spawn` fix itself is straightforward enough to verify by code inspection, and a real regression test would need to simulate the render loop's threading model in a way that risks being flakier than the bug it's guarding against.
