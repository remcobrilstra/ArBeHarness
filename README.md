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

## Local commands

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run
```

CI (`.github/workflows/ci.yml`) runs the same three gates on Linux, macOS, and Windows.
