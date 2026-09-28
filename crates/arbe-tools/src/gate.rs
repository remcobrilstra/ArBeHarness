use arbe_core::{ApprovalDecision, ToolError, ToolInvocation, ToolResult};

use crate::{ApprovalContext, ApprovalPolicy, PolicyOutcome, ToolContext, ToolRegistry};

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
///
/// A session-scoped human decision (`ApprovedForSession` /
/// `AlwaysDeniedForSession`) is recorded into `ctx.session` here, so later
/// invocations of the same tool are decided by the policy without asking.
pub async fn execute_gated(
    registry: &ToolRegistry,
    policy: &dyn ApprovalPolicy,
    ctx: &ApprovalContext,
    invocation: ToolInvocation,
    human_decision: Option<ApprovalDecision>,
    tool_ctx: &ToolContext,
) -> Result<GatedOutcome, ToolError> {
    let executor = registry.get(&invocation.tool_name)?.clone();

    let approved = match (policy.decide(&invocation, ctx), human_decision) {
        (PolicyOutcome::AutoApprove, _) => true,
        (PolicyOutcome::AutoDeny, _) => false,
        (PolicyOutcome::RequiresPrompt, None) => return Ok(GatedOutcome::PendingApproval),
        (PolicyOutcome::RequiresPrompt, Some(decision)) => {
            ctx.session.record(&invocation.tool_name, decision);
            decision.is_approved()
        }
    };

    if !approved {
        return Ok(GatedOutcome::Denied);
    }

    executor
        .execute(invocation, tool_ctx)
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
        async fn execute(
            &self,
            invocation: ToolInvocation,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: invocation.arguments,
                is_error: false,
            })
        }
    }

    use crate::{ToolContext, ToolExecutor};

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
        ApprovalContext::new(mode, vec![], vec![])
    }

    #[tokio::test]
    async fn approve_for_session_executes_later_calls_without_asking() {
        let policy = crate::StandardApprovalPolicy;
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt);
        let first = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            Some(ApprovalDecision::ApprovedForSession),
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(first, GatedOutcome::Executed(_)));

        let second = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            None,
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(second, GatedOutcome::Executed(_)));
    }

    #[tokio::test]
    async fn always_deny_for_session_denies_later_calls_without_asking() {
        let policy = crate::StandardApprovalPolicy;
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt);
        let first = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            Some(ApprovalDecision::AlwaysDeniedForSession),
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(first, GatedOutcome::Denied));

        let second = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            None,
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(second, GatedOutcome::Denied));
    }

    #[tokio::test]
    async fn approve_once_is_not_remembered() {
        let policy = crate::StandardApprovalPolicy;
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt);
        execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            Some(ApprovalDecision::ApprovedOnce),
            &ToolContext::default(),
        )
        .await
        .unwrap();
        let second = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            None,
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(second, GatedOutcome::PendingApproval));
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
            &ToolContext::default(),
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
            &ToolContext::default(),
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
            &ToolContext::default(),
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
            &ToolContext::default(),
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
            &ToolContext::default(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::Denied));
    }
}
