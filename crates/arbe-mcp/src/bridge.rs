use std::sync::Arc;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use arbe_tools::{ToolExecutor, ToolRegistry};
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::client::{McpClient, McpToolInfo};

/// Adapts one MCP server tool to `arbe_tools::ToolExecutor`, so it's
/// indistinguishable from a local tool once registered (overall design
/// §4.4: "MCP tools exposed through unified tool registry"). The client is
/// shared (`Arc<Mutex<..>>`) because a single stdio connection serializes
/// all requests to one server, regardless of how many of its tools get
/// called concurrently.
pub struct McpToolExecutor {
    client: Arc<Mutex<McpClient>>,
    tool_name: String,
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        let mut client = self.client.lock().await;
        let output = client
            .call_tool(&self.tool_name, invocation.arguments)
            .await
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;
        Ok(ToolResult {
            id: invocation.id,
            output,
            is_error: false,
        })
    }
}

/// `server_name/tool_name`, so tools from different MCP servers (or a
/// local tool that happens to share a name) can never collide in the
/// unified registry.
pub fn qualified_tool_name(server_name: &str, tool_name: &str) -> String {
    format!("{server_name}/{tool_name}")
}

/// Registers every tool an already-initialized MCP client reported via
/// `list_tools` into the shared registry under its qualified name.
pub fn register_server_tools(
    registry: &mut ToolRegistry,
    server_name: &str,
    client: Arc<Mutex<McpClient>>,
    tools: Vec<McpToolInfo>,
) {
    for tool in tools {
        let qualified_name = qualified_tool_name(server_name, &tool.name);
        registry.register(
            qualified_name,
            Arc::new(McpToolExecutor {
                client: client.clone(),
                tool_name: tool.name,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifies_tool_names_with_the_server_name() {
        assert_eq!(
            qualified_tool_name("my-server", "search"),
            "my-server/search"
        );
    }
}
