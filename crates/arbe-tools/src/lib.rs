//! Tool registry, approval policy, and execution contracts (harness spec
//! FR-4, overall design §4.6).

pub mod builtin;
pub mod gate;
pub mod policy;
pub mod registry;
pub mod session_approvals;

pub use gate::{GatedOutcome, execute_gated};
pub use policy::{PolicyOutcome, StandardApprovalPolicy};
pub use registry::ToolRegistry;
pub use session_approvals::SessionApprovals;

use std::sync::Arc;

use arbe_core::{ApprovalPolicyMode, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
pub use tokio_util::sync::CancellationToken;

/// The policy config in effect for a decision (harness spec FR-4). Kept
/// separate from `StandardApprovalPolicy` itself so the same policy
/// implementation can be reused across sessions/profiles that configure
/// different modes/lists.
pub struct ApprovalContext {
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
    /// Session-scoped human decisions, recorded by `execute_gated`.
    pub session: SessionApprovals,
    /// Whether an "approve for session" answer also covers
    /// `RiskLevel::High` tools. Off by default: approving `execute` for the
    /// session would otherwise auto-approve *every* later shell command,
    /// whatever it is, so high-risk calls keep prompting each time unless
    /// config explicitly opts in.
    pub session_approval_covers_high_risk: bool,
}

impl ApprovalContext {
    /// A context with empty session memory and the safe high-risk default.
    pub fn new(
        policy_mode: ApprovalPolicyMode,
        allowlist: Vec<String>,
        denylist: Vec<String>,
    ) -> Self {
        Self {
            policy_mode,
            allowlist,
            denylist,
            session: SessionApprovals::new(),
            session_approval_covers_high_risk: false,
        }
    }
}

/// Decides, without asking a human, what should happen to an invocation.
/// `RequiresPrompt` means the runtime must pause and route the decision to
/// a human (TUI-FR-2) — it is not itself a final answer, unlike
/// `arbe_core::ApprovalDecision` which *is* a human's final answer.
pub trait ApprovalPolicy: Send + Sync {
    fn decide(&self, invocation: &ToolInvocation, ctx: &ApprovalContext) -> PolicyOutcome;
}

/// Receives human-readable progress updates from a running tool (e.g. a
/// long command's latest output line), for UIs to show while it runs.
pub type ProgressSink = Arc<dyn Fn(String) + Send + Sync>;

/// Everything a tool execution needs besides its invocation.
#[derive(Clone, Default)]
pub struct ToolContext {
    /// Fires when the turn is cancelled. Long-running tools should stop
    /// promptly (e.g. `execute` kills its child process) and return
    /// `ToolError::Cancelled`; quick tools may ignore it.
    pub cancel: CancellationToken,
    pub progress: Option<ProgressSink>,
}

impl ToolContext {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            progress: None,
        }
    }

    /// Reports progress, if anyone is listening.
    pub fn report(&self, update: impl Into<String>) {
        if let Some(progress) = &self.progress {
            progress(update.into());
        }
    }
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;
}

/// Test-only shorthand: run a tool with a default (never-cancelled)
/// context, so tool unit tests don't each construct one.
#[cfg(test)]
pub(crate) trait ExecuteWithDefaultContext {
    async fn execute_default(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError>;
}

#[cfg(test)]
impl<T: ToolExecutor + ?Sized> ExecuteWithDefaultContext for T {
    async fn execute_default(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        self.execute(invocation, &ToolContext::default()).await
    }
}
