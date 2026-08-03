# ArBeHarness

A high-performance, model-agnostic Rust agent harness with a lightweight TUI, built without external agent-loop frameworks.

See `/docs` for the full design (`v1-overall-design.md`), harness spec (`v1-harness-spec.md`), TUI spec (`v1-tui-spec.md`), and implementation plan (`v1-implementation-plan.md`). See `CLAUDE.md` for current build status and architecture rules.

## Workspace layout

```text
Cargo.toml               # workspace root + `arbeharness` bin
src/main.rs               # bin entrypoint
crates/
  arbe-core/               # domain types, event model, error taxonomy
  arbe-storage/             # ~/.arbe/ filesystem persistence
  arbe-providers/           # model provider trait + adapters
  arbe-memory/              # context assembly + memory strategies
  arbe-tools/               # tool registry + approval policy
  arbe-skills/              # skill manifest loading
  arbe-hooks/               # lifecycle hook system
  arbe-mcp/                 # MCP server config + tool bridge
  arbe-runtime/             # orchestration glue, event bus
  arbe-tui/                 # terminal UI (depends only on arbe-runtime)
```

## Running

`cargo run` launches the TUI. With no configuration it defaults to a local Ollama server (`http://localhost:11434`, model `llama3`) — no API key needed to launch, but you'll need Ollama actually running to get a reply.

To use OpenAI instead, set these environment variables before running:

| Variable | Required | Purpose |
|---|---|---|
| `ARBE_PROVIDER` | yes | set to `openai` |
| `OPENAI_API_KEY` | yes (when provider is `openai`) | your OpenAI API key — never hardcode this, only ever read from env |
| `ARBE_MODEL` | no | defaults to `gpt-5-mini` when provider is `openai` |
| `ARBE_BASE_URL` | no | override the API base URL, e.g. to point at an OpenAI-compatible gateway |
| `ARBE_TEMPERATURE` | no | defaults to `1.0` for `openai` (its reasoning-family models reject any other value), `0.2` otherwise |
| `ARBE_WORKDIR` | no | the project/repo directory the agent works on; defaults to the current directory |

There's also `ARBE_HOME` / `--dev-home <path>`, but that's a different, **development/testing-only** knob — see below.

### Working directory vs. harness home — two different roots

These are easy to conflate but control different things:

- **`ARBE_WORKDIR` / `--workdir <path>`** — the repo/project the agent is actually working *on* (usually your current directory; where file/execute tools will operate once they exist). Shown in the TUI header as `workdir: ...`.
- **`ARBE_HOME` / `--dev-home <path>`** — where the harness's *own* state lives: `sessions/`, `skills/`, `memory/`, `mcp/`, `logs/`. Defaults to `~/.arbe/` (`%USERPROFILE%\.arbe\` on Windows). **This is a development/testing knob, not something a normal run needs to touch** — it exists so tests and quick manual runs don't pollute your real `~/.arbe/` with scratch sessions. `--dev-home <path>` is equivalent to `ARBE_HOME` but doesn't require exporting an env var first, e.g.:

  ```bash
  cargo run -- --dev-home ./scratch-arbe-home --workdir ../some-other-repo
  ```

PowerShell:

```powershell
$env:ARBE_PROVIDER = "openai"
$env:OPENAI_API_KEY = "sk-..."
cargo run
```

bash/zsh:

```bash
ARBE_PROVIDER=openai OPENAI_API_KEY=sk-... cargo run
```

Once it's running:
- Type a message and press `Enter` to chat (response streams token-by-token).
- `Ctrl+L` clears the transcript view (no data loss — it's still on disk under `~/.arbe/sessions/<session-id>/`).
- `Ctrl+C` quits.
- `/tool <name> <json-args>` proposes a tool call — approve/deny it with `y`/`n`/`a`/`d` in the modal that appears. This exists because no provider currently returns structured tool calls in its response (see `docs/v1-status.md`), so there's no automatic way to trigger the approval flow from a real model reply yet; `/tool` is the manual stand-in. Every session registers these builtin tools by default, sandboxed to `ARBE_WORKDIR`:

  | Tool | Args | Notes |
  |---|---|---|
  | `read_file` | `{"path": "...", "start_line"?, "end_line"?}` | line range is optional and 1-indexed |
  | `write_file` | `{"path": "...", "content": "..."}` | creates parent dirs, atomic write |
  | `edit_file` | `{"path": "...", "find": "...", "replace": "...", "replace_all"?}` | errors if `find` isn't found (or matches more than once without `replace_all: true`) |
  | `list_dir` | `{"path"?: "..."}` | defaults to the workdir root |
  | `glob` | `{"pattern": "...", "path"?: "..."}` | e.g. `{"pattern":"**/*.rs"}` |
  | `grep` | `{"pattern": "...", "path"?: "...", "case_insensitive"?}` | pattern is a regex |
  | `execute` | `{"command": "...", "timeout_secs"?}` | runs in a shell, cwd = workdir — **highest risk**, always goes through approval |

  e.g. `/tool list_dir {}` or `/tool read_file {"path":"Cargo.toml"}`.

There is no session-resume picker in the TUI yet — every run starts a new session (`docs/v1-status.md` tracks this as a known gap).

## Local commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run
```

CI (`.github/workflows/ci.yml`) runs the same three gates on Linux, macOS, and Windows.
