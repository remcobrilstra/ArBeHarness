//! Model Context Protocol client (harness spec FR-8). Servers are
//! configured in `config.toml` (`[mcp.servers.<name>]`), started or
//! connected per session, and their tools registered into the same
//! `ToolRegistry` — and approval gate — as builtin tools, named
//! `<server>__<tool>`.
//!
//! Transports: stdio (the harness runs the server process) and streamable
//! HTTP. Requests are matched to responses by id, so calls can overlap;
//! timeouts and cancellation are sent to the server as
//! `notifications/cancelled`; a crashed stdio server is restarted on the
//! next call.

pub mod bridge;
pub mod client;
pub mod config;
mod http;
pub mod manager;
pub mod protocol;
mod stdio;

pub use bridge::{McpToolExecutor, qualified_tool_name, unique_tool_name};
pub use client::{CallOutcome, McpClient, McpToolInfo};
pub use config::{McpServerConfig, McpServerSettings, TransportConfig};
pub use manager::{McpManager, McpServer, ServerStatus, ToolSink};
pub use protocol::McpError;
