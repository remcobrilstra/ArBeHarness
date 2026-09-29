use std::sync::Arc;

use arbe_core::{ApprovalDecision, ToolError, ToolInvocation, ToolResult};

use crate::{
    ApprovalContext, ApprovalPolicy, PolicyOutcome, ToolContext, ToolExecutor, ToolRegistry,
    ToolRun,
};

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

/// Proof that an invocation passed the approval gate. Only [`authorize`]
/// can create one (its fields are private), and executing a tool through
/// the gate requires one — so approval and execution can happen at
/// different times (e.g. approve several calls one by one, then run them
/// concurrently) without opening a path that skips approval.
pub struct Authorized {
    invocation: ToolInvocation,
    executor: Arc<dyn ToolExecutor>,
}

impl Authorized {
    pub fn invocation(&self) -> &ToolInvocation {
        &self.invocation
    }

    /// Whether this tool may run concurrently with others (see
    /// `ToolExecutor::parallel_safe`).
    pub fn parallel_safe(&self) -> bool {
        self.executor.parallel_safe()
    }

    /// Runs the approved call. This is where the [`ToolContext`] every
    /// executor requires is issued — the only place it can be.
    pub async fn execute(self, run: ToolRun) -> Result<ToolResult, ToolError> {
        let tool_ctx = ToolContext::issue(run);
        self.executor.execute(self.invocation, &tool_ctx).await
    }
}

/// The gate's verdict on one invocation.
pub enum Authorization {
    Approved(Authorized),
    Denied(ToolInvocation),
    /// The policy wants a human decision: call [`authorize`] again with
    /// `Some(decision)` once there is one.
    NeedsHuman(ToolInvocation),
}

/// The approval half of the gate — the single choke point every tool
/// invocation must pass through (overall design's non-negotiable
/// approval-gate rule). `human_decision` is `None` on the first pass; after
/// `NeedsHuman`, the runtime collects a decision and calls again with it.
///
/// A session-scoped human decision (`ApprovedForSession` /
/// `AlwaysDeniedForSession`) is recorded into `ctx.session` here, so later
/// invocations of the same tool are decided by the policy without asking.
pub fn authorize(
    registry: &ToolRegistry,
    policy: &dyn ApprovalPolicy,
    ctx: &ApprovalContext,
    invocation: ToolInvocation,
    human_decision: Option<ApprovalDecision>,
) -> Result<Authorization, ToolError> {
    let executor = registry.get(&invocation.tool_name)?.clone();
    let subject = executor.subject(&invocation.arguments);

    let approved = match (
        policy.decide(&invocation, subject.as_deref(), ctx),
        human_decision,
    ) {
        (PolicyOutcome::AutoApprove, _) => true,
        (PolicyOutcome::AutoDeny, _) => false,
        (PolicyOutcome::RequiresPrompt, _) if !executor.requires_approval() => true,
        (PolicyOutcome::RequiresPrompt, None) => {
            return Ok(Authorization::NeedsHuman(invocation));
        }
        (PolicyOutcome::RequiresPrompt, Some(decision)) => {
            ctx.session.record(
                &invocation.tool_name,
                subject.as_deref(),
                invocation.risk,
                decision,
            );
            decision.is_approved()
        }
    };

    Ok(if approved {
        Authorization::Approved(Authorized {
            invocation,
            executor,
        })
    } else {
        Authorization::Denied(invocation)
    })
}

/// [`authorize`] and, if approved, execute — in one call.
pub async fn execute_gated(
    registry: &ToolRegistry,
    policy: &dyn ApprovalPolicy,
    ctx: &ApprovalContext,
    invocation: ToolInvocation,
    human_decision: Option<ApprovalDecision>,
    run: ToolRun,
) -> Result<GatedOutcome, ToolError> {
    match authorize(registry, policy, ctx, invocation, human_decision)? {
        Authorization::Approved(authorized) => {
            authorized.execute(run).await.map(GatedOutcome::Executed)
        }
        Authorization::Denied(_) => Ok(GatedOutcome::Denied),
        Authorization::NeedsHuman(_) => Ok(GatedOutcome::PendingApproval),
    }
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
                attachments: Vec::new(),
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

    /// Like `EchoExecutor`, but acts on nothing, so never asks.
    struct QuietExecutor;

    #[async_trait]
    impl ToolExecutor for QuietExecutor {
        async fn execute(
            &self,
            invocation: ToolInvocation,
            ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            EchoExecutor.execute(invocation, ctx).await
        }
        fn requires_approval(&self) -> bool {
            false
        }
    }

    #[test]
    fn tools_that_act_on_nothing_skip_the_prompt_but_not_deny_rules_or_dry_run() {
        let policy = crate::StandardApprovalPolicy;
        let mut r = ToolRegistry::new();
        r.register("echo", Arc::new(QuietExecutor));
        let approved = |c: &ApprovalContext| {
            matches!(
                authorize(&r, &policy, c, invocation(), None).unwrap(),
                Authorization::Approved(_)
            )
        };
        assert!(approved(&ctx(ApprovalPolicyMode::AlwaysPrompt)));
        assert!(!approved(&ctx(ApprovalPolicyMode::DryRunOnly)));
        let denied = ApprovalContext::new(
            ApprovalPolicyMode::AlwaysPrompt,
            vec![],
            vec!["echo".into()],
        );
        assert!(!approved(&denied));
    }

    #[tokio::test]
    async fn authorize_asks_for_a_human_then_hands_back_an_executable_approval() {
        let policy = crate::StandardApprovalPolicy;
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt);
        let Authorization::NeedsHuman(invocation) =
            authorize(&registry(), &policy, &c, invocation(), None).unwrap()
        else {
            panic!("expected NeedsHuman");
        };
        let Authorization::Approved(approved) = authorize(
            &registry(),
            &policy,
            &c,
            invocation,
            Some(ApprovalDecision::ApprovedOnce),
        )
        .unwrap() else {
            panic!("expected Approved");
        };
        assert!(approved.parallel_safe());
        let result = approved.execute(ToolRun::default()).await.unwrap();
        assert_eq!(result.output, json!({"a": 1}));
    }

    #[test]
    fn authorize_denies_and_returns_the_invocation() {
        let policy = crate::StandardApprovalPolicy;
        let outcome = authorize(
            &registry(),
            &policy,
            &ctx(ApprovalPolicyMode::DryRunOnly),
            invocation(),
            None,
        )
        .unwrap();
        assert!(matches!(outcome, Authorization::Denied(inv) if inv.tool_name == "echo"));
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
        )
        .await
        .unwrap();
        let second = execute_gated(
            &registry(),
            &policy,
            &c,
            invocation(),
            None,
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
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
            ToolRun::default(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatedOutcome::Denied));
    }
}
