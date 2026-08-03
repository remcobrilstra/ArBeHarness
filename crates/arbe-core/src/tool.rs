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
