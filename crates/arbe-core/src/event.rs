use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{SessionId, ToolCallId, TurnId};
use crate::tool::{ApprovalDecision, RiskLevel, ToolResult};
use crate::usage::{StopReason, Usage};

/// A published `RuntimeEvent` plus its position in the bus's stream.
/// `seq` increases by exactly one per published event, so a consumer can
/// detect dropped events (a gap) without relying on the transport to tell
/// it — important once events cross a process boundary (headless mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u64,
    pub event: RuntimeEvent,
}

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
    /// A model request failed transiently (rate limit, overload, timeout)
    /// and will be retried after `delay_ms`.
    ProviderRetrying {
        turn_id: TurnId,
        attempt: u32,
        delay_ms: u64,
        reason: String,
    },
    /// A fragment of the model's reasoning, for UIs that show it.
    ThinkingDelta {
        turn_id: TurnId,
        delta: String,
    },
    /// The model has started emitting a tool call. `provider_call_id` is
    /// the provider's id for it — the harness's own `ToolCallId` is only
    /// assigned once the call is complete (`ToolCallProposed`).
    ToolUseStarted {
        turn_id: TurnId,
        provider_call_id: String,
        tool_name: String,
    },
    /// A fragment of a streaming tool call's JSON arguments, so a UI can
    /// show them forming.
    ToolUseInputDelta {
        turn_id: TurnId,
        provider_call_id: String,
        partial_json: String,
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
        tool_name: String,
        result: ToolResult,
    },
    ToolCallDenied {
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        tool_name: String,
        reason: String,
    },
    /// A running tool's progress report (e.g. a long command's output).
    ToolProgress {
        turn_id: TurnId,
        tool_call_id: ToolCallId,
        update: String,
    },
    /// Token usage after an inference call: this turn's running total and
    /// the session's.
    UsageUpdated {
        session_id: SessionId,
        turn_id: TurnId,
        turn: Usage,
        session: Usage,
    },
    /// An MCP server connected and its tools were registered.
    McpServerConnected {
        server: String,
        tools: usize,
    },
    /// An MCP server couldn't be started or reached; its tools are
    /// unavailable this session.
    McpServerFailed {
        server: String,
        reason: String,
    },
    /// A hook failed (error, bad output, timeout) and was skipped.
    HookFailed {
        hook: String,
        reason: String,
    },
    /// Older context was compacted to fit the budget.
    CompactionPerformed {
        session_id: SessionId,
        turn_id: Option<TurnId>,
        compacted_messages: u64,
    },
    /// The turn was cancelled before completing; what it produced so far
    /// has been persisted.
    TurnCancelled {
        session_id: SessionId,
        turn_id: TurnId,
    },
    TurnCompleted {
        session_id: SessionId,
        turn_id: TurnId,
        /// Why the turn ended — a normal answer, or a loop guard.
        stop_reason: StopReason,
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
    /// Cancel the session's in-flight turn, if any.
    CancelTurn {
        session_id: SessionId,
    },
}
