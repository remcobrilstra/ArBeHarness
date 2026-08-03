use arbe_core::{ApprovalDecision, ToolError, ToolInvocation, ToolResult};

use crate::{ApprovalContext, ApprovalPolicy, PolicyOutcome, ToolRegistry};

/// What happened to a gated invocation. Deliberately distinct from
/// `Result<ToolResult, ToolError>` alone: `PendingApproval` is not a
/// failure, it's a signal that the runtime must pause the loop
/// (`LoopPhase::ToolApproval`) and re-drive this same call once a human
/// decision arrives (harness spec FR-4: "runtime pause/resume for
/// approval").
#[derive(Debug)]
pub enum GatedOutcome {
    Executed(ToolResult),
    Denied,
    PendingApproval,
}

/// The single choke point every tool invocation must pass through — no
/// tool ever calls `ToolExecutor::execute` directly (overall design's
/// non-negotiable approval-gate rule). `human_decision` is `None` on the
/// first pass through a turn; once the runtime collects a decision from a
/// `PendingApproval` outcome, it re-calls this with `Some(decision)`.
pub async fn execute_gated(
    registry: &ToolRegistry,
    policy: &dyn ApprovalPolicy,
    ctx: &ApprovalContext,
    invocation: ToolInvocation,
    human_decision: Option<ApprovalDecision>,
) -> Result<GatedOutcome, ToolError> {
    let executor = registry.get(&invocation.tool_name)?.clone();

    let approved = match (policy.decide(&invocation, ctx), human_decision) {
        (PolicyOutcome::AutoApprove, _) => true,
        (PolicyOutcome::AutoDeny, _) => false,
        (PolicyOutcome::RequiresPrompt, None) => return Ok(GatedOutcome::PendingApproval),
        (PolicyOutcome::RequiresPrompt, Some(decision)) => decision.is_approved(),
    };

    if !approved {
        return Ok(GatedOutcome::Denied);
    }

    executor
        .execute(invocation)
        .await
        .map(GatedOutcome::Executed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ApprovalPolicyMode, RiskLevel, ToolCallId, TurnId};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct EchoExecutor;

    #[async_trait]
    impl ToolExecutor for EchoExecutor {
        async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: invocation.arguments,
                is_error: false,
            })
        }
    }

    use crate::ToolExecutor;

    fn invocation() -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: "echo".to_string(),
            arguments: json!({"a": 1}),
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register("echo", Arc::new(EchoExecutor));
        r
    }

    fn ctx(mode: ApprovalPolicyMode) -> ApprovalContext {
        ApprovalContext {
            policy_mode: mode,
            allowlist: vec![],
            denylist: vec![],
        }
    }

    #[tokio::test]
    async fn unknown_tool_fails_before_any_policy_check() {
        let empty = ToolRegistry::new();
        let policy = crate::StandardApprovalPolicy;
        let err = execute_gated(
            &empty,
            &policy,
            &ctx(ApprovalPolicyMode::AlwaysPrompt),
            invocation(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn always_prompt_without_a_decision_pends() {
        let policy = crate::StandardApprovalPolicy;
        let outcome = execute_gated(
            &registry(),
            &policy,
            &ctx(ApprovalPolicyMode::AlwaysPrompt),
            invocation(),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::PendingApproval));
    }

    #[tokio::test]
    async fn approved_once_after_a_pending_prompt_executes() {
        let policy = crate::StandardApprovalPolicy;
        let outcome = execute_gated(
            &registry(),
            &policy,
            &ctx(ApprovalPolicyMode::AlwaysPrompt),
            invocation(),
            Some(ApprovalDecision::ApprovedOnce),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::Executed(_)));
    }

    #[tokio::test]
    async fn denied_once_after_a_pending_prompt_is_denied_not_executed() {
        let policy = crate::StandardApprovalPolicy;
        let outcome = execute_gated(
            &registry(),
            &policy,
            &ctx(ApprovalPolicyMode::AlwaysPrompt),
            invocation(),
            Some(ApprovalDecision::DeniedOnce),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::Denied));
    }

    #[tokio::test]
    async fn dry_run_only_denies_without_ever_calling_the_executor() {
        let policy = crate::StandardApprovalPolicy;
        let outcome = execute_gated(
            &registry(),
            &policy,
            &ctx(ApprovalPolicyMode::DryRunOnly),
            invocation(),
            None,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::Denied));
    }
}
