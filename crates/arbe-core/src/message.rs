use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tool::RequestedToolCall;

/// Role distinction used both for context assembly and TUI rendering (TUI-FR-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Set on an `Assistant` message that requested one or more tool
    /// calls (mirrors OpenAI's `message.tool_calls`) — `None` for every
    /// other message. `#[serde(default)]` so turns persisted before this
    /// field existed still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<RequestedToolCall>>,
    /// Set on a `Tool` message: which requested call (`RequestedToolCall::id`)
    /// this message's `content` is the result of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            timestamp: Utc::now(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// An assistant message that requested tool calls rather than (or in
    /// addition to) producing text content.
    pub fn assistant_tool_calls(tool_calls: Vec<RequestedToolCall>) -> Self {
        Self {
            tool_calls: Some(tool_calls),
            ..Self::new(Role::Assistant, "")
        }
    }

    /// A `role: "tool"` message carrying one tool's result back to the
    /// model, per `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(tool_call_id.into()),
            ..Self::new(Role::Tool, content)
        }
    }
}
