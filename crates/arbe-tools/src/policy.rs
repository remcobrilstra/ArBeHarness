use arbe_core::{ApprovalPolicyMode, RiskLevel};

use crate::{ApprovalContext, ApprovalPolicy};

/// What an `ApprovalPolicy` decided, before any human gets involved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    AutoApprove,
    AutoDeny,
    RequiresPrompt,
}

/// The four policy modes from harness spec FR-4, with [`ToolRule`]s
/// (`tool` or `tool(pattern)`) for the allow and deny lists:
/// - `AlwaysPrompt`: every call asks a human.
/// - `AllowlistAuto`: calls matching an allow rule run without asking;
///   everything else asks.
/// - `DenylistBlock`: everything runs without asking except what a deny
///   rule refuses.
/// - `DryRunOnly`: nothing runs.
///
/// In every mode:
/// - a deny rule refuses a matching call (so `execute(git push*)` is
///   enforced whatever the mode);
/// - a `High`-risk call never runs without asking just because of the mode
///   or a bare tool name — only a *specific* allow rule (one with a
///   pattern, e.g. `execute(cargo test*)`) can auto-approve it;
/// - session decisions layer on top: "deny for session" refuses, "approve
///   for session" turns a prompt into an approval (for a `High`-risk call
///   only when the session rule is for that exact call, or config sets
///   `session_approval_covers_high_risk`). They never override a deny.
///
/// [`ToolRule`]: crate::rules::ToolRule
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardApprovalPolicy;

impl ApprovalPolicy for StandardApprovalPolicy {
    fn decide(
        &self,
        invocation: &arbe_core::ToolInvocation,
        subject: Option<&str>,
        ctx: &ApprovalContext,
    ) -> PolicyOutcome {
        let name = invocation.tool_name.as_str();
        if ctx.policy_mode == ApprovalPolicyMode::DryRunOnly
            || ctx.session.is_denied(name, subject)
            || ctx.denylist.iter().any(|r| r.matches(name, subject))
        {
            return PolicyOutcome::AutoDeny;
        }

        let high_risk = invocation.risk == RiskLevel::High;
        let allow_rule = ctx.allowlist.iter().find(|r| r.matches(name, subject));
        let outcome = match ctx.policy_mode {
            ApprovalPolicyMode::AllowlistAuto => match allow_rule {
                Some(rule) if !high_risk || rule.is_specific() => PolicyOutcome::AutoApprove,
                _ => PolicyOutcome::RequiresPrompt,
            },
            ApprovalPolicyMode::DenylistBlock if !high_risk => PolicyOutcome::AutoApprove,
            ApprovalPolicyMode::DenylistBlock => match allow_rule {
                Some(rule) if rule.is_specific() => PolicyOutcome::AutoApprove,
                _ => PolicyOutcome::RequiresPrompt,
            },
            ApprovalPolicyMode::AlwaysPrompt | ApprovalPolicyMode::DryRunOnly => {
                PolicyOutcome::RequiresPrompt
            }
        };

        if outcome == PolicyOutcome::RequiresPrompt
            && let Some(rule) = ctx.session.allowed_by(name, subject)
            && (!high_risk || rule.is_specific() || ctx.session_approval_covers_high_risk)
        {
            return PolicyOutcome::AutoApprove;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ApprovalDecision, RiskLevel, ToolCallId, ToolInvocation, TurnId};
    use serde_json::json;

    fn call(tool_name: &str, risk: RiskLevel) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: tool_name.to_string(),
            arguments: json!({}),
            risk,
            rationale: None,
        }
    }

    fn ctx(mode: ApprovalPolicyMode, allow: &[&str], deny: &[&str]) -> ApprovalContext {
        ApprovalContext::new(
            mode,
            allow.iter().map(|s| s.to_string()).collect(),
            deny.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn decide(
        c: &ApprovalContext,
        tool: &str,
        subject: Option<&str>,
        risk: RiskLevel,
    ) -> PolicyOutcome {
        StandardApprovalPolicy.decide(&call(tool, risk), subject, c)
    }

    use PolicyOutcome::*;
    use RiskLevel::*;

    #[test]
    fn always_prompt_never_auto_decides() {
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt, &["read_file"], &[]);
        assert_eq!(decide(&c, "read_file", Some("a"), Low), RequiresPrompt);
    }

    #[test]
    fn allowlist_auto_approves_matching_calls_only() {
        let c = ctx(
            ApprovalPolicyMode::AllowlistAuto,
            &["read_file", "write_file(src/*)"],
            &[],
        );
        assert_eq!(decide(&c, "read_file", Some("x"), Low), AutoApprove);
        assert_eq!(
            decide(&c, "write_file", Some("src/lib.rs"), Medium),
            AutoApprove
        );
        assert_eq!(
            decide(&c, "write_file", Some("Cargo.toml"), Medium),
            RequiresPrompt
        );
        assert_eq!(decide(&c, "execute", Some("ls"), High), RequiresPrompt);
    }

    #[test]
    fn denylist_block_approves_everything_not_denied() {
        let c = ctx(
            ApprovalPolicyMode::DenylistBlock,
            &[],
            &["write_file(.git/*)"],
        );
        assert_eq!(
            decide(&c, "write_file", Some("src/a.rs"), Medium),
            AutoApprove
        );
        assert_eq!(
            decide(&c, "write_file", Some(".git/config"), Medium),
            AutoDeny
        );
    }

    #[test]
    fn deny_rules_apply_in_every_mode() {
        for mode in [
            ApprovalPolicyMode::AlwaysPrompt,
            ApprovalPolicyMode::AllowlistAuto,
        ] {
            let c = ctx(mode, &["execute(*)"], &["execute(git push*)"]);
            assert_eq!(
                decide(&c, "execute", Some("git push origin main"), High),
                AutoDeny
            );
        }
    }

    #[test]
    fn high_risk_needs_a_specific_allow_rule_to_skip_the_prompt() {
        let bare = ctx(ApprovalPolicyMode::AllowlistAuto, &["execute"], &[]);
        assert_eq!(
            decide(&bare, "execute", Some("cargo test"), High),
            RequiresPrompt
        );
        let specific = ctx(
            ApprovalPolicyMode::AllowlistAuto,
            &["execute(cargo test*)"],
            &[],
        );
        assert_eq!(
            decide(&specific, "execute", Some("cargo test -p x"), High),
            AutoApprove
        );
        assert_eq!(
            decide(&specific, "execute", Some("cargo build"), High),
            RequiresPrompt
        );
        // Same in denylist mode: high risk isn't auto-approved by default.
        let deny_mode = ctx(ApprovalPolicyMode::DenylistBlock, &[], &[]);
        assert_eq!(
            decide(&deny_mode, "execute", Some("ls"), High),
            RequiresPrompt
        );
        let deny_mode = ctx(ApprovalPolicyMode::DenylistBlock, &["execute(ls*)"], &[]);
        assert_eq!(
            decide(&deny_mode, "execute", Some("ls -la"), High),
            AutoApprove
        );
    }

    #[test]
    fn dry_run_denies_everything_even_session_approved() {
        let c = ctx(ApprovalPolicyMode::DryRunOnly, &["read_file"], &[]);
        c.session
            .record("read_file", None, Low, ApprovalDecision::ApprovedForSession);
        assert_eq!(decide(&c, "read_file", None, Low), AutoDeny);
    }

    #[test]
    fn session_approval_turns_a_prompt_into_an_approval() {
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt, &[], &[]);
        c.session.record(
            "read_file",
            Some("a"),
            Low,
            ApprovalDecision::ApprovedForSession,
        );
        assert_eq!(decide(&c, "read_file", Some("b"), Low), AutoApprove);
        assert_eq!(decide(&c, "write_file", Some("b"), Medium), RequiresPrompt);
    }

    #[test]
    fn session_approval_of_a_high_risk_call_covers_only_that_exact_call() {
        let c = ctx(ApprovalPolicyMode::AlwaysPrompt, &[], &[]);
        c.session.record(
            "execute",
            Some("cargo test"),
            High,
            ApprovalDecision::ApprovedForSession,
        );
        assert_eq!(decide(&c, "execute", Some("cargo test"), High), AutoApprove);
        assert_eq!(
            decide(&c, "execute", Some("cargo test; rm -rf /"), High),
            RequiresPrompt
        );
    }

    #[test]
    fn session_deny_wins_and_session_approval_never_overrides_a_deny_rule() {
        let c = ctx(ApprovalPolicyMode::DenylistBlock, &[], &["rm_rf"]);
        c.session
            .record("grep", None, Low, ApprovalDecision::AlwaysDeniedForSession);
        assert_eq!(decide(&c, "grep", None, Low), AutoDeny);
        c.session
            .record("rm_rf", None, Low, ApprovalDecision::ApprovedForSession);
        assert_eq!(decide(&c, "rm_rf", None, Low), AutoDeny);
    }

    #[test]
    fn the_high_risk_session_flag_widens_session_approvals() {
        let mut c = ctx(ApprovalPolicyMode::AlwaysPrompt, &[], &[]);
        c.session.record(
            "mcp__drop_table",
            None,
            High,
            ApprovalDecision::ApprovedForSession,
        );
        // Without a subject there's no exact call to pin the approval to,
        // so a high-risk tool keeps asking — unless the flag is set.
        assert_eq!(decide(&c, "mcp__drop_table", None, High), RequiresPrompt);
        let c2 = ctx(ApprovalPolicyMode::AlwaysPrompt, &[], &[]);
        c2.session.record(
            "execute",
            Some("ls"),
            High,
            ApprovalDecision::ApprovedForSession,
        );
        assert_eq!(decide(&c2, "execute", Some("pwd"), High), RequiresPrompt);
        c.session_approval_covers_high_risk = true;
        assert_eq!(decide(&c, "mcp__drop_table", None, High), AutoApprove);
    }
}
