//! Tool registry, approval policy, and execution contracts (harness spec
//! FR-4, overall design §4.6).

pub mod builtin;
pub mod gate;
pub mod policy;
pub mod registry;

pub use gate::{GatedOutcome, execute_gated};
pub use policy::{PolicyOutcome, StandardApprovalPolicy};
pub use registry::ToolRegistry;

use arbe_core::{ApprovalPolicyMode, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;

/// The policy config in effect for a decision (harness spec FR-4). Kept
/// separate from `StandardApprovalPolicy` itself so the same policy
/// implementation can be reused across sessions/profiles that configure
/// different modes/lists.
pub struct ApprovalContext {
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
}

/// Decides, without asking a human, what should happen to an invocation.
/// `RequiresPrompt` means the runtime must pause and route the decision to
/// a human (TUI-FR-2) — it is not itself a final answer, unlike
/// `arbe_core::ApprovalDecision` which *is* a human's final answer.
pub trait ApprovalPolicy: Send + Sync {
    fn decide(&self, invocation: &ToolInvocation, ctx: &ApprovalContext) -> PolicyOutcome;
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError>;
}
