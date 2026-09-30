//! Exposes an MCP server's tools as ordinary `ToolExecutor`s, so they go
//! through the same registry and approval gate as builtin tools.

use std::sync::Arc;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use arbe_tools::{ToolContext, ToolDescription, ToolExecutor};
use async_trait::async_trait;
use serde_json::Value;

use crate::client::McpToolInfo;
use crate::manager::McpServer;
use crate::protocol::McpError;

/// Longest tool name model APIs accept (OpenAI and Anthropic: 64).
const MAX_TOOL_NAME: usize = 64;

/// The registry name for a server's tool: `server__tool`, with anything
/// outside `[A-Za-z0-9_-]` replaced by `_` and cut to 64 characters —
/// providers reject other names (including the `/` a path-like name would
/// suggest).
pub fn qualified_tool_name(server: &str, tool: &str) -> String {
    let raw = format!("{server}__{tool}");
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(MAX_TOOL_NAME)
        .collect()
}

/// `name`, or — if `taken` says it's in use (sanitizing and the 64-char
/// cut can map different tools, even of different servers, to one name:
/// `a.b`/`a_b`, or server `foo` + tool `bar__baz` vs. server `foo__bar` +
/// tool `baz`) — the first free `name_2`, `name_3`, ..., still within the
/// length limit. Registering over a taken name would silently replace
/// another tool.
pub fn unique_tool_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| {
            let suffix = format!("_{n}");
            let base: String = name.chars().take(MAX_TOOL_NAME - suffix.len()).collect();
            format!("{base}{suffix}")
        })
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range always finds a free name")
}

pub struct McpToolExecutor {
    server: Arc<McpServer>,
    tool: McpToolInfo,
}

impl McpToolExecutor {
    pub fn new(server: Arc<McpServer>, tool: McpToolInfo) -> Self {
        Self { server, tool }
    }
}

#[async_trait]
impl ToolExecutor for McpToolExecutor {
    /// The server's `readOnlyHint` annotation — its word, not verified.
    fn read_only(&self) -> bool {
        self.tool.read_only
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let client = self.server.client().await.map_err(|e| {
            ToolError::RuntimeFailure(format!(
                "mcp server {:?} unavailable: {e}",
                self.server.name()
            ))
        })?;
        let outcome = client
            .call_tool(&self.tool.name, invocation.arguments, Some(&ctx.cancel))
            .await
            .map_err(|e| match e {
                McpError::Cancelled => ToolError::Cancelled,
                McpError::Timeout(_) => ToolError::Timeout,
                other => ToolError::RuntimeFailure(other.to_string()),
            })?;
        Ok(ToolResult {
            id: invocation.id,
            output: Value::String(outcome.text),
            is_error: outcome.is_error,
            attachments: Vec::new(),
        })
    }

    /// Only tools the server marks read-only run alongside other calls;
    /// anything else might have side effects that conflict.
    fn parallel_safe(&self) -> bool {
        self.tool.read_only
    }

    fn description(&self) -> ToolDescription {
        ToolDescription {
            description: self.tool.description.clone(),
            parameters: self.tool.input_schema.clone(),
        }
    }

    /// From the server's hints: read-only → low, destructive → high,
    /// otherwise medium. (Hints are the server's claim, not a guarantee;
    /// approval policy still applies either way.)
    fn default_risk(&self) -> RiskLevel {
        if self.tool.read_only {
            RiskLevel::Low
        } else if self.tool.destructive {
            RiskLevel::High
        } else {
            RiskLevel::Medium
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_name_gets_a_numbered_suffix_within_the_limit() {
        assert_eq!(unique_tool_name("gh__search", |_| false), "gh__search");
        let taken = ["gh__search", "gh__search_2"];
        assert_eq!(
            unique_tool_name("gh__search", |n| taken.contains(&n)),
            "gh__search_3"
        );
        let long = "x".repeat(MAX_TOOL_NAME);
        let renamed = unique_tool_name(&long, |n| n == long);
        assert_eq!(renamed.len(), MAX_TOOL_NAME);
        assert!(renamed.ends_with("_2"));
        // Two servers whose names collide once qualified.
        assert_eq!(
            qualified_tool_name("foo", "bar__baz"),
            qualified_tool_name("foo__bar", "baz")
        );
    }

    #[test]
    fn qualified_names_are_safe_for_every_provider() {
        assert_eq!(
            qualified_tool_name("github", "search_issues"),
            "github__search_issues"
        );
        assert_eq!(
            qualified_tool_name("fs", "read/file.v2"),
            "fs__read_file_v2"
        );
        let long = qualified_tool_name("server", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with("server__"));
    }
}
