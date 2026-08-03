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

#[tokio::main]
async fn main() {
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
