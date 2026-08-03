//! Tool registry, approval policy, and execution contracts (harness spec
//! FR-4, overall design §4.6). The registry and concrete executors land in
//! Phase 4; this crate currently defines only the shared contract.

use arbe_core::{ApprovalDecision, ApprovalPolicyMode, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;

pub struct ApprovalContext {
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
}

pub trait ApprovalPolicy: Send + Sync {
    fn decide(&self, invocation: &ToolInvocation, ctx: &ApprovalContext) -> ApprovalDecision;
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError>;
}
