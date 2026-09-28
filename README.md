# ArBeHarness

A high-performance, model-agnostic Rust agent harness with a lightweight TUI, built without external agent-loop frameworks.

End users: start with [`docs/user-guide.md`](docs/user-guide.md). See `/docs` for the full design (`v1-overall-design.md`), harness spec (`v1-harness-spec.md`), TUI spec (`v1-tui-spec.md`), and implementation plan (`v1-implementation-plan.md`). See `CLAUDE.md` for current build status and architecture rules.

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

```bash
cargo run --release                          # local Ollama (qwen2.5-coder:3b), no API key needed
ARBE_PROVIDER=anthropic ANTHROPIC_API_KEY=... cargo run --release
cargo run --release -- --workdir ../some-repo
```

**See [`docs/user-guide.md`](docs/user-guide.md)** for the full end-user documentation: CLI arguments, environment variables, providers, keybindings, chat commands, tools and approvals, instruction/skill files, sessions, where files are stored, and file formats.

## Local commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run
```

CI (`.github/workflows/ci.yml`) runs the same three gates on Linux, macOS, and Windows.
