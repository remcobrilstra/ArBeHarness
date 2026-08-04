use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::{ToolCallId, TurnId};

/// Coarse risk indicator shown in the approval UX (TUI-FR-2, harness spec FR-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// A typed, normalized tool invocation produced by output interpretation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: ToolCallId,
    pub source_turn: TurnId,
    pub tool_name: String,
    pub arguments: Value,
    pub risk: RiskLevel,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: ToolCallId,
    pub output: Value,
    pub is_error: bool,
}

/// A tool's name/description/JSON-schema, sent to a provider that supports
/// tool calling (`ProviderCapabilities::tool_calls`) so the model knows
/// what it can invoke and how to shape arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema (object type) describing the arguments this tool
    /// accepts — passed through to the provider largely as-is.
    pub parameters: Value,
}

/// A tool call the model asked for, as surfaced by a provider's response.
/// `id` is the provider's own call id (e.g. OpenAI's `tool_calls[].id`) —
/// distinct from `arbe_core::ToolCallId`, which is the harness's internal
/// identifier assigned once the call is turned into a `ToolInvocation`.
/// The provider's id has to be threaded back into the follow-up
/// `role: "tool"` message so the API can match the result to the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestedToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Policy modes from harness spec FR-4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicyMode {
    AlwaysPrompt,
    AllowlistAuto,
    DenylistBlock,
    DryRunOnly,
}

/// Outcome of an approval decision, including the session-scoped variants
/// from TUI-FR-2 (approve/deny for the rest of the session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    ApprovedOnce,
    DeniedOnce,
    ApprovedForSession,
    AlwaysDeniedForSession,
}

impl ApprovalDecision {
    pub fn is_approved(self) -> bool {
        matches!(self, Self::ApprovedOnce | Self::ApprovedForSession)
    }
}
