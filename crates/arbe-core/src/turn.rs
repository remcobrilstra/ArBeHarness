use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, TurnId};
use crate::message::Message;
use crate::tool::{ToolInvocation, ToolResult};

/// One record in a session's append-only `turns.jsonl` log: a single pass
/// through the agent loop (harness spec FR-1, FR-3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub session_id: SessionId,
    pub index: u64,
    pub user_message: Option<Message>,
    pub assistant_message: Option<Message>,
    pub tool_calls: Vec<ToolInvocation>,
    pub tool_results: Vec<ToolResult>,
    pub created_at: DateTime<Utc>,
}

impl Turn {
    pub fn new(session_id: SessionId, index: u64) -> Self {
        Self {
            id: TurnId::new(),
            session_id,
            index,
            user_message: None,
            assistant_message: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            created_at: Utc::now(),
        }
    }
}
