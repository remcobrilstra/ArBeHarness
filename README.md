# ArBeHarness

A model-agnostic agent harness in Rust: a coding (or general) agent that works on a project directory with tools, asks before it acts, remembers, and can be driven from a terminal UI, a script, another program, or embedded as a library.

- **Any model** — OpenAI, Anthropic, local Ollama, or any OpenAI-compatible server (xAI, vLLM, LM Studio, OpenRouter, …); switching is a setting, not a code change.
- **Tools, gated** — read/write/edit files, search, run commands, a task list, persistent memory, and subagents. Every tool call goes through an approval gate: you approve, deny, or allow for the session, or configure rules (`execute(cargo test*)`).
- **Extensible** — MCP servers (stdio and HTTP), skills loaded on demand, command hooks that can veto tool calls, per-project instruction files (`AGENTS.md`, `CLAUDE.md`).
- **Long sessions** — old tool output is pruned, history is summarized by the model when it fills the context, and everything is saved as it happens: a crash loses at most the reply that was streaming.
- **Four ways to use it** — the terminal UI; `--print` for one prompt from a script; `--headless` JSON-RPC for editors and apps; or the `arbe_runtime::Harness` library API.

## Build and run

Requires a Rust toolchain (edition 2024). No prebuilt binaries yet.

```bash
cargo build --release          # produces target/release/arbeharness
```

With no configuration it uses a local [Ollama](https://ollama.com) server:

```bash
ollama pull qwen2.5-coder:3b
cargo run --release -- --workdir ../my-project
```

Or a hosted model:

```bash
ARBE_PROVIDER=anthropic ANTHROPIC_API_KEY=... cargo run --release
ARBE_PROVIDER=openai OPENAI_API_KEY=... cargo run --release
ARBE_PROVIDER=openai_compatible ARBE_BASE_URL=https://api.x.ai/v1 ARBE_API_KEY=... ARBE_MODEL=grok-4.7 cargo run --release
cargo run --release -- login grok     # Grok subscription, then:
cargo run --release -- --profile grok-subscription
```

Small local models are fine for trying it out, but they're weak at multi-step tool use; a hosted model makes a much better agent.

## Other ways to run it

```bash
# One prompt, answer on stdout; approve reads, deny everything else:
arbeharness --workdir ../repo --print "Which tests cover the parser?" --approve reads

# Continue a saved session, as JSON lines (events, then a result):
arbeharness --resume <session-id> --print "Now fix it" --approve all --output json

# Serve JSON-RPC 2.0 on stdin/stdout for another program:
arbeharness --headless --workdir ../repo
```

Embedding (see `crates/arbe-runtime/examples/embed.rs`):

```rust
let harness = Harness::builder().project_dir(".").build()?;
let session = harness.new_session()?;
let mut turn = session.send("Summarize README.md");
while let Some(event) = turn.next_event().await { /* stream, answer approvals */ }
let answer = turn.finish().await?;
```

## Documentation

- **[User guide](docs/user-guide.md)** — every option, setting, keybinding, tool, file location and format, the JSON-RPC protocol, and troubleshooting.
- [v2 status](docs/v2-status.md) — what's verified and what isn't.
- [v2 implementation plan](docs/v2-implementation-plan.md) — the roadmap and progress log.
- [CHANGELOG](CHANGELOG.md).
- Design background: `docs/v1-overall-design.md`, `docs/v1-harness-spec.md`, `docs/v1-tui-spec.md`.

## Development

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check                                     # licenses, advisories
cargo bench -p arbe-memory --bench context           # also arbe-storage, arbe-providers
```

Live tests against real models are `#[ignore]`d; see the user guide and `.github/workflows/live.yml`. CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on Linux, macOS and Windows, plus `cargo-deny`.

### Workspace

```text
src/                  the `arbeharness` binary: CLI, --print, --headless
crates/
  arbe-core/          domain types, events, errors
  arbe-storage/       ~/.arbe persistence (sessions, memory, instructions)
  arbe-providers/     model providers (OpenAI, Anthropic, Ollama, compatible)
  arbe-memory/        context assembly, pruning, compaction strategies
  arbe-tools/         tool registry, approval gate, builtin tools
  arbe-skills/        skill files
  arbe-hooks/         lifecycle hooks
  arbe-mcp/           MCP client (stdio, HTTP)
  arbe-runtime/       the agent loop, config, subagents, library API
  arbe-tui/           terminal UI (depends only on arbe-runtime)
```

License: proprietary, all rights reserved — see [LICENSE](LICENSE). No permission to use, copy, modify or distribute is granted without written permission.
