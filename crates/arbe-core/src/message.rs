use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::tool::RequestedToolCall;

/// Role distinction used both for context assembly and TUI rendering (TUI-FR-1).
///
/// `Tool` marks a message that carries tool results back to the model.
/// Providers map it to their own convention (OpenAI: `role: "tool"`, one
/// message per result; Anthropic: `tool_result` blocks in a user message).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Where an image's bytes come from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded bytes, inline.
    Base64 { data: String },
    /// A URL the provider fetches itself.
    Url { url: String },
}

/// One typed piece of a message. Every serious provider API now models a
/// message as a list of these rather than a single string, and the
/// harness needs to carry them losslessly between providers, persistence
/// and replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
        media_type: String,
    },
    /// The model asking for a tool call. `id` is the provider's own call id
    /// (or one the adapter synthesized when the provider gives none); it is
    /// threaded back on the matching `ToolResult`.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default)]
        is_error: bool,
    },
    /// Model reasoning. `signature` is the provider's integrity token
    /// (Anthropic requires it echoed back verbatim on later turns).
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// A provider-specific block the harness doesn't interpret (e.g.
    /// redacted thinking), round-tripped verbatim to the provider that
    /// produced it and dropped for any other provider.
    Opaque {
        provider: String,
        data: Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    /// v1 persisted this as a plain string; `content_compat` still accepts
    /// that shape so v1 sessions keep loading.
    #[serde(deserialize_with = "content_compat")]
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    /// Prompt-caching hint: the provider may cache the prompt prefix up to
    /// and including this message. Providers without caching ignore it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cache_breakpoint: bool,
}

impl Message {
    /// A message consisting of a single text block (none if `text` is empty).
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        let text = text.into();
        let content = if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentBlock::Text { text }]
        };
        Self::with_blocks(role, content)
    }

    pub fn with_blocks(role: Role, content: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content,
            timestamp: Utc::now(),
            cache_breakpoint: false,
        }
    }

    /// An assistant message that requested tool calls rather than (or in
    /// addition to) producing text content.
    pub fn assistant_tool_calls(tool_calls: Vec<RequestedToolCall>) -> Self {
        Self::with_blocks(
            Role::Assistant,
            tool_calls
                .into_iter()
                .map(|c| ContentBlock::ToolUse {
                    id: c.id,
                    name: c.name,
                    input: c.arguments,
                })
                .collect(),
        )
    }

    /// A `Tool` message carrying one tool's (text) result back to the
    /// model, matched to its request by `tool_use_id`.
    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::tool_result_blocks(tool_use_id, vec![ContentBlock::text(content)], false)
    }

    pub fn tool_result_blocks(
        tool_use_id: impl Into<String>,
        content: Vec<ContentBlock>,
        is_error: bool,
    ) -> Self {
        Self::with_blocks(
            Role::Tool,
            vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content,
                is_error,
            }],
        )
    }

    /// All text blocks, concatenated. Tool calls, thinking and images are
    /// not included — this is "what the message says", for display and for
    /// providers/strategies that only deal in text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for block in &self.content {
            if let ContentBlock::Text { text } = block {
                out.push_str(text);
            }
        }
        out
    }

    /// The tool calls this message requests, in order.
    pub fn tool_uses(&self) -> Vec<RequestedToolCall> {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(RequestedToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: input.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    pub fn has_tool_uses(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
}

/// Accepts either the v2 block list or the v1 plain-string content.
fn content_compat<'de, D>(deserializer: D) -> Result<Vec<ContentBlock>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Compat {
        Blocks(Vec<ContentBlock>),
        Text(String),
    }
    Ok(match Compat::deserialize(deserializer)? {
        Compat::Blocks(blocks) => blocks,
        Compat::Text(text) if text.is_empty() => Vec::new(),
        Compat::Text(text) => vec![ContentBlock::Text { text }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_with_text_is_one_text_block_and_empty_text_is_no_blocks() {
        assert_eq!(
            Message::new(Role::User, "hi").content,
            vec![ContentBlock::text("hi")]
        );
        assert!(Message::new(Role::Assistant, "").content.is_empty());
    }

    #[test]
    fn text_concatenates_only_text_blocks() {
        let m = Message::with_blocks(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    text: "hmm".into(),
                    signature: None,
                },
                ContentBlock::text("a"),
                ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "t".into(),
                    input: json!({}),
                },
                ContentBlock::text("b"),
            ],
        );
        assert_eq!(m.text(), "ab");
    }

    #[test]
    fn tool_calls_round_trip_through_blocks() {
        let call = RequestedToolCall {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a"}),
        };
        let m = Message::assistant_tool_calls(vec![call]);
        assert!(m.has_tool_uses());
        let uses = m.tool_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].id, "c1");
        assert_eq!(uses[0].arguments, json!({"path": "a"}));
    }

    #[test]
    fn serializes_blocks_with_a_type_tag() {
        let m = Message::tool_result("c1", "ok");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["content"][0]["type"], "tool_result");
        assert_eq!(v["content"][0]["tool_use_id"], "c1");
        assert_eq!(v["content"][0]["content"][0]["text"], "ok");
        // Default cache flag is omitted from the wire/persisted form.
        assert!(v.get("cache_breakpoint").is_none());
    }

    #[test]
    fn v2_messages_round_trip() {
        let m = Message::with_blocks(
            Role::User,
            vec![
                ContentBlock::text("look"),
                ContentBlock::Image {
                    source: ImageSource::Url {
                        url: "https://x/y.png".into(),
                    },
                    media_type: "image/png".into(),
                },
            ],
        );
        let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back.content, m.content);
    }

    #[test]
    fn deserializes_the_v1_plain_string_shape() {
        let v1 = r#"{"role":"assistant","content":"hello","timestamp":"2026-08-01T00:00:00Z"}"#;
        let m: Message = serde_json::from_str(v1).unwrap();
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(m.text(), "hello");

        let empty = r#"{"role":"user","content":"","timestamp":"2026-08-01T00:00:00Z"}"#;
        let m: Message = serde_json::from_str(empty).unwrap();
        assert!(m.content.is_empty());
    }
}
