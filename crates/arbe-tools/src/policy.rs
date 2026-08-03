use arbe_core::ApprovalPolicyMode;

use crate::{ApprovalContext, ApprovalPolicy};

/// What an `ApprovalPolicy` decided, before any human gets involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    AutoApprove,
    AutoDeny,
    RequiresPrompt,
}

/// The four policy modes from harness spec FR-4, applied uniformly
/// regardless of tool:
/// - `AlwaysPrompt`: every invocation requires a human decision.
/// - `AllowlistAuto`: allowlisted tools auto-approve; everything else still
///   requires a prompt (the allowlist only ever *widens* what's automatic,
///   it never blocks).
/// - `DenylistBlock`: denylisted tools auto-deny; everything else
///   auto-approves (a permissive default that blocks only known-bad tools).
/// - `DryRunOnly`: nothing ever executes — every invocation auto-denies,
///   since there is no dry-run execution engine yet to "simulate" it.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardApprovalPolicy;

impl ApprovalPolicy for StandardApprovalPolicy {
    fn decide(
        &self,
        invocation: &arbe_core::ToolInvocation,
        ctx: &ApprovalContext,
    ) -> PolicyOutcome {
        match ctx.policy_mode {
            ApprovalPolicyMode::AlwaysPrompt => PolicyOutcome::RequiresPrompt,
            ApprovalPolicyMode::AllowlistAuto => {
                if ctx.allowlist.iter().any(|t| t == &invocation.tool_name) {
                    PolicyOutcome::AutoApprove
                } else {
                    PolicyOutcome::RequiresPrompt
                }
            }
            ApprovalPolicyMode::DenylistBlock => {
                if ctx.denylist.iter().any(|t| t == &invocation.tool_name) {
                    PolicyOutcome::AutoDeny
                } else {
                    PolicyOutcome::AutoApprove
                }
            }
            ApprovalPolicyMode::DryRunOnly => PolicyOutcome::AutoDeny,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{RiskLevel, ToolCallId, ToolInvocation, TurnId};
    use serde_json::json;

    fn invocation(tool_name: &str) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: tool_name.to_string(),
            arguments: json!({}),
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    fn ctx(mode: ApprovalPolicyMode, allowlist: &[&str], denylist: &[&str]) -> ApprovalContext {
        ApprovalContext {
            policy_mode: mode,
            allowlist: allowlist.iter().map(|s| s.to_string()).collect(),
            denylist: denylist.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn always_prompt_never_auto_decides() {
        let policy = StandardApprovalPolicy;
        let outcome = policy.decide(
            &invocation("bash"),
            &ctx(ApprovalPolicyMode::AlwaysPrompt, &[], &[]),
        );
        assert_eq!(outcome, PolicyOutcome::RequiresPrompt);
    }

    #[test]
    fn allowlist_auto_approves_listed_tools_only() {
        let policy = StandardApprovalPolicy;
        let allow_ctx = ctx(ApprovalPolicyMode::AllowlistAuto, &["read_file"], &[]);
        assert_eq!(
            policy.decide(&invocation("read_file"), &allow_ctx),
            PolicyOutcome::AutoApprove
        );
        assert_eq!(
            policy.decide(&invocation("bash"), &allow_ctx),
            PolicyOutcome::RequiresPrompt
        );
    }

    #[test]
    fn denylist_block_denies_listed_tools_and_approves_the_rest() {
        let policy = StandardApprovalPolicy;
        let deny_ctx = ctx(ApprovalPolicyMode::DenylistBlock, &[], &["rm_rf"]);
        assert_eq!(
            policy.decide(&invocation("rm_rf"), &deny_ctx),
            PolicyOutcome::AutoDeny
        );
        assert_eq!(
            policy.decide(&invocation("read_file"), &deny_ctx),
            PolicyOutcome::AutoApprove
        );
    }

    #[test]
    fn dry_run_only_always_denies() {
        let policy = StandardApprovalPolicy;
        let outcome = policy.decide(
            &invocation("anything"),
            &ctx(ApprovalPolicyMode::DryRunOnly, &[], &[]),
        );
        assert_eq!(outcome, PolicyOutcome::AutoDeny);
    }
}
