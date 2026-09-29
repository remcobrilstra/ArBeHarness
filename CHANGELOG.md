# Changelog

## Unreleased — 0.2.0 (branch `v2`)

A rebuild of the harness core: a streaming agent loop on typed content blocks, three new providers, context management for long sessions, and ways to use the harness beyond the terminal UI. See [`docs/v2-status.md`](docs/v2-status.md) for what's verified and what isn't yet.

### Models

- Providers: OpenAI, **Anthropic** (with extended thinking), **Ollama** (tool calling on), and **any OpenAI-compatible server** (xAI, vLLM, LM Studio, OpenRouter, …). Switching is a setting.
- Streaming everywhere, including tool calls and reasoning (`reasoning_content` from compatible servers shows as thinking).
- Retries with backoff for rate limits, overload, timeouts and server errors 500/502/503/529, honoring `Retry-After`.
- A reply that breaks off partway is an error, never a silently truncated answer.
- Tool calls some small models write as plain JSON text (e.g. `qwen2.5-coder`) are recognized.
- Default local models: `qwen2.5-coder:3b` (coding) and `llama3.2:3b` (general).
- A per-model catalog (context window, vision, thinking), correctable in config.

### Agent

- Multi-round tool use with parallel execution of read-only tools; tool errors and denials go back to the model instead of failing the turn.
- Cancel a turn at any point (`Esc`); what it did so far is kept.
- Loop guards: tool-round limit, per-turn token limit, repeated identical calls.
- Every message is saved as it's produced; a crashed turn is recovered on resume.
- **Subagents**: a `task` tool that hands a job to a child agent with a fresh context; its approvals come to you as usual.
- **Images**: `read_file` returns PNG/JPEG/GIF/WebP files as images to models that can see them.
- Persistent **memory** (`remember` tool, global and per-project notes).
- Nested instruction files: an `AGENTS.md`/`agent.md`/`CLAUDE.md` in a subfolder applies once the agent works there.

### Long sessions

- Old tool output is pruned first; then, with `memory_strategy = "compact_summary"`, the model summarizes older turns (also on demand with `/compact`); whole turns are dropped only as a last resort. Tool calls and their results are never split.

### Configuration & safety

- TOML config files (global, project, and `--config` extras) with **profiles** (`coding`, `general`, or your own) and environment/CLI overrides.
- Project config is untrusted by default: it can't change endpoints, keys, approvals, MCP servers or hooks unless the project is listed in `trusted_projects`.
- Approval rules with patterns (`execute(cargo test*)`), approve-for-session, and deny rules that always apply.
- Running a tool without passing the approval gate no longer compiles (outside the tools crate).
- Secrets (API keys, credential headers, secret-looking environment variables) are redacted from tool output.
- API keys from a command (`api_key_command`).

### Extensions

- **MCP** client over stdio and streamable HTTP, with reconnection and tool-list updates.
- **Skills**, loaded on demand by the model or always included.
- **Command hooks** at every stage of a turn; `before_tool_execute` can rewrite or veto a call; `on_approval_requested` for "needs your attention" notifications. Every payload includes the session id.

### Ways to run it

- `--print "<prompt>"`: one turn without a UI, with `--approve none|reads|all`, `--output text|json` and meaningful exit codes.
- `--headless`: JSON-RPC 2.0 over stdio (sessions, turns, approvals, cancellation, events).
- `--resume <id>`, `--prompt`, `--name`, `--profile`, `--provider`, `--model`, `--config`, `--version`, `--help`; unknown options are errors.
- A library API: `arbe_runtime::Harness::builder()` → `Session::send` → a stream of events and the answer.
- `meta.json` records each session's `workdir`, git `branch`, live `activity` and `pid`, so other programs can track sessions.

### Terminal UI

- The model's reasoning in a collapsible `[thinking]` entry (`Ctrl+T`); live preview of tool arguments while they stream; subagent activity nested under its `task` call.

### Development

- 471 tests, plus live tests against real models (`#[ignore]`d; a manual CI workflow), HTTP-level provider tests, criterion benchmarks, and `cargo-deny` checks.
- Updated rustls to 0.23.45 (RUSTSEC-2026-0285).

## 0.1.0

The v1 prototype: session persistence, OpenAI and Ollama providers, builtin tools behind an approval gate, skills, hooks, a first MCP client, and the terminal UI. See [`docs/v1-status.md`](docs/v1-status.md).
