use std::sync::{Arc, Mutex};

use arbe_core::{ApprovalDecision, RiskLevel};

use crate::rules::ToolRule;

/// Session-scoped approval memory: the human's "approve for the rest of
/// the session" / "always deny for the rest of the session" answers
/// (TUI-FR-2), kept as [`ToolRule`]s.
///
/// - Approving a low/medium-risk call for the session approves the *tool*.
/// - Approving a high-risk call for the session approves only that *exact
///   call* (same tool, same subject — e.g. the same command line), since
///   approving `execute` itself would approve any command.
/// - Denying for the session denies the tool.
///
/// Recorded by the gate whenever a human decision resolves a prompt, and
/// consulted by `StandardApprovalPolicy` on every later call. Cheaply
/// `Clone` (an `Arc` around a small mutex) so one memory is shared by
/// everything holding the session's `ApprovalContext`.
#[derive(Debug, Clone, Default)]
pub struct SessionApprovals {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    allowed: Vec<ToolRule>,
    denied: Vec<ToolRule>,
}

impl SessionApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers a session-scoped decision about a call. Once-only
    /// decisions aren't remembered; a later session-scoped decision for a
    /// tool replaces an earlier opposite one.
    pub fn record(
        &self,
        tool_name: &str,
        subject: Option<&str>,
        risk: RiskLevel,
        decision: ApprovalDecision,
    ) {
        let mut inner = self.lock();
        match decision {
            ApprovalDecision::ApprovedForSession => {
                inner.denied.retain(|r| !r.matches(tool_name, subject));
                let rule = if risk == RiskLevel::High {
                    ToolRule::exact(tool_name, subject)
                } else {
                    ToolRule::exact(tool_name, None)
                };
                if !inner.allowed.contains(&rule) {
                    inner.allowed.push(rule);
                }
            }
            ApprovalDecision::AlwaysDeniedForSession => {
                inner.allowed.retain(|r| !r.matches(tool_name, subject));
                inner.denied.push(ToolRule::exact(tool_name, None));
            }
            ApprovalDecision::ApprovedOnce | ApprovalDecision::DeniedOnce => {}
        }
    }

    /// The session rule approving this call, if any.
    pub fn allowed_by(&self, tool_name: &str, subject: Option<&str>) -> Option<ToolRule> {
        self.lock()
            .allowed
            .iter()
            .find(|r| r.matches(tool_name, subject))
            .cloned()
    }

    pub fn is_denied(&self, tool_name: &str, subject: Option<&str>) -> bool {
        self.lock()
            .denied
            .iter()
            .any(|r| r.matches(tool_name, subject))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // The critical sections above can't panic, so poisoning can't
        // happen in practice; recover the data rather than propagate.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOW: RiskLevel = RiskLevel::Low;
    const HIGH: RiskLevel = RiskLevel::High;

    #[test]
    fn once_decisions_are_not_remembered() {
        let s = SessionApprovals::new();
        s.record("read_file", Some("a"), LOW, ApprovalDecision::ApprovedOnce);
        s.record("execute", Some("ls"), HIGH, ApprovalDecision::DeniedOnce);
        assert!(s.allowed_by("read_file", Some("a")).is_none());
        assert!(!s.is_denied("execute", Some("ls")));
    }

    #[test]
    fn approving_a_low_risk_call_approves_the_tool() {
        let s = SessionApprovals::new();
        s.record(
            "read_file",
            Some("a.rs"),
            LOW,
            ApprovalDecision::ApprovedForSession,
        );
        assert!(s.allowed_by("read_file", Some("b.rs")).is_some());
        assert!(s.allowed_by("write_file", Some("a.rs")).is_none());
    }

    #[test]
    fn approving_a_high_risk_call_approves_only_that_exact_call() {
        let s = SessionApprovals::new();
        s.record(
            "execute",
            Some("cargo test"),
            HIGH,
            ApprovalDecision::ApprovedForSession,
        );
        let rule = s.allowed_by("execute", Some("cargo test")).unwrap();
        assert!(rule.is_specific());
        assert!(
            s.allowed_by("execute", Some("cargo test && rm -rf /"))
                .is_none()
        );
        assert!(s.allowed_by("execute", Some("git push")).is_none());
    }

    #[test]
    fn deny_replaces_approve_and_vice_versa() {
        let s = SessionApprovals::new();
        s.record("grep", None, LOW, ApprovalDecision::ApprovedForSession);
        s.record("grep", None, LOW, ApprovalDecision::AlwaysDeniedForSession);
        assert!(s.allowed_by("grep", None).is_none());
        assert!(s.is_denied("grep", Some("x")));
        s.record("grep", None, LOW, ApprovalDecision::ApprovedForSession);
        assert!(!s.is_denied("grep", None));
    }

    #[test]
    fn clones_share_the_same_memory() {
        let a = SessionApprovals::new();
        let b = a.clone();
        a.record("glob", None, LOW, ApprovalDecision::ApprovedForSession);
        assert!(b.allowed_by("glob", None).is_some());
    }
}
