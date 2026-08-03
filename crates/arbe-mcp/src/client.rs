use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::McpServerConfig;
use crate::protocol::{McpError, build_notification, build_request, parse_response};

#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
}

/// A connection to one MCP server over stdio (overall design §4.4). Owns
/// the child process for the server's lifetime; dropping the client kills
/// the process (`Child`'s default drop behavior... actually `Child` does
/// *not* kill on drop by default, so callers that need that should call
/// `kill_on_drop(true)` via `spawn`, which this does).
///
/// There is no live-process integration test for this type — the
/// request/response framing it depends on (`protocol.rs`) is fully unit
/// tested without a real process; wiring it to an actual MCP server is
/// exercised manually / in a future end-to-end test once a reference
/// server is available in CI.
pub struct McpClient {
    #[allow(dead_code)] // kept alive only so the child is killed when this drops
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    pub fn spawn(config: &McpServerConfig) -> Result<Self, McpError> {
        let mut child = Command::new(&config.command)
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| McpError::Transport(format!("failed to spawn {}: {e}", config.command)))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child had no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child had no stdout".to_string()))?;

        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        })
    }

    async fn write_line(&mut self, value: &Value) -> Result<(), McpError> {
        let mut line = serde_json::to_string(value).map_err(|e| McpError::Parse(e.to_string()))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| McpError::Transport(e.to_string()))
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = build_request(id, method, params);
        self.write_line(&req).await?;

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .await
            .map_err(|e| McpError::Transport(e.to_string()))?;
        if line.is_empty() {
            return Err(McpError::Transport(
                "MCP server closed the connection".to_string(),
            ));
        }
        parse_response(line.trim(), id)
    }

    /// Performs the `initialize` handshake and sends the
    /// `notifications/initialized` follow-up notification MCP requires.
    pub async fn initialize(&mut self) -> Result<(), McpError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "arbeharness", "version": env!("CARGO_PKG_VERSION") },
            }),
        )
        .await?;
        let notif = build_notification("notifications/initialized", json!({}));
        self.write_line(&notif).await
    }

    pub async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .ok_or_else(|| McpError::Parse("tools/list result missing tools array".to_string()))?;

        tools
            .iter()
            .map(|t| {
                let name = t
                    .get("name")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| McpError::Parse("tool entry missing name".to_string()))?
                    .to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or_default()
                    .to_string();
                Ok(McpToolInfo { name, description })
            })
            .collect()
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }
}
