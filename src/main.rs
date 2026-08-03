use std::sync::Arc;

use arbe_tui::arbe_runtime::arbe_core::{ToolError, ToolInvocation, ToolResult};
use arbe_tui::arbe_runtime::arbe_tools::ToolExecutor;
use arbe_tui::arbe_runtime::{Agent, EventBus, RuntimeConfig};

/// A trivial local tool so the TUI's `/tool` demo command has something
/// real to run without requiring an MCP server or API key: `/tool echo
/// {"text":"hi"}` proposes a call, and approving it returns the arguments
/// back as the result.
struct EchoTool;

#[async_trait::async_trait]
impl ToolExecutor for EchoTool {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        Ok(ToolResult {
            id: invocation.id,
            output: invocation.arguments,
            is_error: false,
        })
    }
}

/// Reads `--home <path>` / `--home=<path>` from argv, if present. Lets a
/// test run point at a scratch directory instead of the real `~/.arbe/`
/// without having to export an env var first — sets `ARBE_HOME` itself
/// under the hood, so it's equivalent to (and overrides) that env var.
fn parse_home_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--home=") {
            return Some(value.to_string());
        }
        if arg == "--home" {
            return args.next();
        }
    }
    None
}

fn main() {
    // Set ARBE_HOME (if overridden via --home) before the tokio runtime —
    // and the worker threads that come with it — is created, so this
    // mutation can never race with another thread reading/writing the env.
    if let Some(home) = parse_home_arg() {
        unsafe {
            std::env::set_var("ARBE_HOME", home);
        }
    }

    tokio::runtime::Runtime::new()
        .expect("failed to start the tokio runtime")
        .block_on(run());
}

async fn run() {
    let config = RuntimeConfig::from_env();
    let store = arbe_tui::arbe_runtime::arbe_storage::SessionStore::new();
    let events = Arc::new(EventBus::default());

    let mut agent = match Agent::create(&config, store, events) {
        Ok(agent) => agent,
        Err(err) => {
            eprintln!("failed to start ArBeHarness: {err}");
            std::process::exit(1);
        }
    };
    agent.register_tool("echo", Arc::new(EchoTool));

    let handle = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || arbe_tui::run(agent, handle))
        .await
        .expect("TUI task panicked");

    if let Err(err) = result {
        eprintln!("TUI exited with an error: {err}");
        std::process::exit(1);
    }
}
