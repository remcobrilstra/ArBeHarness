use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, TurnId};
use crate::message::{Message, Role};
use crate::tool::{ToolInvocation, ToolResult};
use crate::usage::{StopReason, Usage};

/// The `Turn` record format written by this version of the harness.
/// v1 (no `schema_version` field) stored only `user_message` +
/// `assistant_message`; v2 stores the turn's full message sequence.
pub const TURN_SCHEMA_VERSION: u32 = 2;

/// One record in a session's append-only `turns.jsonl` log: a single pass
/// through the agent loop (harness spec FR-1, FR-3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "TurnWire")]
pub struct Turn {
    pub schema_version: u32,
    pub id: TurnId,
    pub session_id: SessionId,
    pub index: u64,
    /// Every message the turn added to the conversation, in order: the
    /// user's message, then each assistant message (text and/or tool
    /// uses) and tool-result message, ending with the final answer.
    pub messages: Vec<Message>,
    /// Audit record of the tool calls the harness handled this turn
    /// (risk level, harness-side ids), alongside their results.
    pub tool_calls: Vec<ToolInvocation>,
    pub tool_results: Vec<ToolResult>,
    /// Provider-reported token usage, summed over every inference call.
    pub usage: Usage,
    /// Why the turn's final inference stopped. `None` for v1 records.
    pub stop_reason: Option<StopReason>,
    pub created_at: DateTime<Utc>,
}

impl Turn {
    pub fn new(session_id: SessionId, index: u64) -> Self {
        Self {
            schema_version: TURN_SCHEMA_VERSION,
            id: TurnId::new(),
            session_id,
            index,
            messages: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            usage: Usage::default(),
            stop_reason: None,
            created_at: Utc::now(),
        }
    }

    /// The message that started the turn.
    pub fn user_message(&self) -> Option<&Message> {
        self.messages.iter().find(|m| m.role == Role::User)
    }

    /// The turn's final assistant message (its answer).
    pub fn final_assistant_message(&self) -> Option<&Message> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
    }
}

/// A summary that stands in for the turns up to and including
/// `through_turn_index` when building context (the turns themselves stay
/// in `turns.jsonl`). A later compaction's summary already covers earlier
/// ones, so only the latest is ever used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Compaction {
    pub through_turn_index: u64,
    pub summary: String,
    /// Tokens the summarization call itself used.
    #[serde(default)]
    pub usage: Usage,
    pub created_at: DateTime<Utc>,
}

/// Accepts both record formats: v2's `messages`, and v1's separate
/// `user_message`/`assistant_message` (converted into `messages`).
#[derive(Deserialize)]
struct TurnWire {
    #[serde(default = "v1_schema")]
    schema_version: u32,
    id: TurnId,
    session_id: SessionId,
    index: u64,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    user_message: Option<Message>,
    #[serde(default)]
    assistant_message: Option<Message>,
    #[serde(default)]
    tool_calls: Vec<ToolInvocation>,
    #[serde(default)]
    tool_results: Vec<ToolResult>,
    #[serde(default)]
    usage: Usage,
    #[serde(default)]
    stop_reason: Option<StopReason>,
    created_at: DateTime<Utc>,
}

fn v1_schema() -> u32 {
    1
}

impl From<TurnWire> for Turn {
    fn from(w: TurnWire) -> Self {
        let mut messages = w.messages;
        if messages.is_empty() {
            messages.extend(w.user_message);
            messages.extend(w.assistant_message);
        }
        Self {
            schema_version: w.schema_version,
            id: w.id,
            session_id: w.session_id,
            index: w.index,
            messages,
            tool_calls: w.tool_calls,
            tool_results: w.tool_results,
            usage: w.usage,
            stop_reason: w.stop_reason,
            created_at: w.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_turn_round_trips_with_its_full_message_sequence() {
        let mut turn = Turn::new(SessionId::new(), 3);
        turn.messages = vec![
            Message::new(Role::User, "list files"),
            Message::assistant_tool_calls(vec![crate::RequestedToolCall {
                id: "c1".into(),
                name: "list_dir".into(),
                arguments: serde_json::json!({}),
            }]),
            Message::tool_result("c1", "a.txt"),
            Message::new(Role::Assistant, "There's a.txt."),
        ];
        turn.stop_reason = Some(StopReason::EndTurn);

        let json = serde_json::to_string(&turn).unwrap();
        assert!(json.contains("\"schema_version\":2"));
        assert!(!json.contains("user_message"));
        let back: Turn = serde_json::from_str(&json).unwrap();
        assert_eq!(back.messages.len(), 4);
        assert_eq!(back.user_message().unwrap().text(), "list files");
        assert_eq!(
            back.final_assistant_message().unwrap().text(),
            "There's a.txt."
        );
        assert_eq!(back.stop_reason, Some(StopReason::EndTurn));
    }

    #[test]
    fn reads_a_v1_turn_record() {
        let v1 = r#"{
            "id":"6f1c7e1a-5a8a-4e53-9d7f-1f6f2d6a9b10",
            "session_id":"0c0c6c0e-2a2b-4c4d-8e8f-909192939495",
            "index":0,
            "user_message":{"role":"user","content":"hello","timestamp":"2026-08-01T00:00:00Z"},
            "assistant_message":{"role":"assistant","content":"hi there","timestamp":"2026-08-01T00:00:01Z"},
            "tool_calls":[],
            "tool_results":[],
            "created_at":"2026-08-01T00:00:00Z"
        }"#;
        let turn: Turn = serde_json::from_str(v1).unwrap();
        assert_eq!(turn.schema_version, 1);
        assert_eq!(turn.messages.len(), 2);
        assert_eq!(turn.user_message().unwrap().text(), "hello");
        assert_eq!(turn.final_assistant_message().unwrap().text(), "hi there");
        assert_eq!(turn.usage, Usage::default());
        assert!(turn.stop_reason.is_none());
    }
}
