//! MCP server discovery, stdio JSON-RPC client, and tool-registry
//! bridging (harness spec FR-8, overall design §4.4).

pub mod bridge;
pub mod client;
pub mod config;
pub mod protocol;

pub use bridge::{McpToolExecutor, qualified_tool_name, register_server_tools};
pub use client::{McpClient, McpToolInfo};
pub use config::{enabled_servers, load_servers_file};
pub use protocol::McpError;

use serde::{Deserialize, Serialize};

/// One entry from `~/.arbe/mcp/servers.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
}
