//! MCP server discovery and tool-registry bridging (harness spec FR-8,
//! overall design §4.4). Handshake/negotiation and the bridge land in
//! Phase 5; this crate currently defines only the config shape.

use serde::{Deserialize, Serialize};

/// One entry from `~/.arbe/mcp/servers.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
}
