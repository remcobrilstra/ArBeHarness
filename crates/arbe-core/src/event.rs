use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{SessionId, ToolCallId, TurnId};
use crate::tool::{ApprovalDecision, RiskLevel, ToolResult};

/// Events emitted by the runtime for UI/debug tooling to consume
/// (TUI spec §5, harness spec FR-10). The TUI must never depend on
/// anything but this contract plus `RuntimeCommand`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    SessionStarted {
        session_id: SessionId,
    },
    TurnStarted {
        session_id: SessionId,
        turn_id: TurnId,
    },
    ContextBuilt {
        turn_id: TurnId,
        estimated_tokens: u64,
    },
    ModelStreamChunk {
        turn_id: TurnId,
        delta: String,
    },
    ToolCallProposed {
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        tool_name: String,
        arguments: Value,
        risk: RiskLevel,
    },
    ToolApprovalRequested {
        turn_id: TurnId,
        tool_call_id: ToolCallId,
    },
    ToolExecuted {
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        result: ToolResult,
    },
    TurnCompleted {
        session_id: SessionId,
        turn_id: TurnId,
    },
    RuntimeError {
        turn_id: Option<TurnId>,
        reason: String,
    },
}

/// Commands the TUI (or any other client) sends to the runtime (TUI spec §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeCommand {
    SubmitUserMessage {
        session_id: SessionId,
        content: String,
    },
    ApproveToolCall {
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    },
    DenyToolCall {
        tool_call_id: ToolCallId,
        decision: ApprovalDecision,
    },
    CreateSession {
        profile: String,
    },
    ResumeSession {
        session_id: SessionId,
    },
    TerminateSession {
        session_id: SessionId,
    },
}
