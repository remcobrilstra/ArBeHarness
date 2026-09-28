use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use arbe_core::ApprovalDecision;

/// Session-scoped approval memory: the human's "approve for the rest of
/// the session" / "always deny for the rest of the session" answers
/// (TUI-FR-2), keyed by tool name.
///
/// Recorded by `execute_gated` whenever a human decision resolves a
/// `RequiresPrompt`, and consulted by `StandardApprovalPolicy` on every
/// later invocation. Cheaply `Clone` (an `Arc` around a small, uncontended
/// mutex) so the same memory can be shared by every clone of an
/// `ApprovalContext` for one session, and so recording works through the
/// `&ApprovalContext` the gate is handed rather than requiring `&mut`.
#[derive(Debug, Clone, Default)]
pub struct SessionApprovals {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    allowed: HashSet<String>,
    denied: HashSet<String>,
}

impl SessionApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers a session-scoped decision for `tool_name`. The once-only
    /// variants are deliberately not remembered. A later session-scoped
    /// decision for the same tool replaces an earlier opposite one.
    pub fn record(&self, tool_name: &str, decision: ApprovalDecision) {
        let mut inner = self.lock();
        match decision {
            ApprovalDecision::ApprovedForSession => {
                inner.denied.remove(tool_name);
                inner.allowed.insert(tool_name.to_string());
            }
            ApprovalDecision::AlwaysDeniedForSession => {
                inner.allowed.remove(tool_name);
                inner.denied.insert(tool_name.to_string());
            }
            ApprovalDecision::ApprovedOnce | ApprovalDecision::DeniedOnce => {}
        }
    }

    pub fn is_allowed(&self, tool_name: &str) -> bool {
        self.lock().allowed.contains(tool_name)
    }

    pub fn is_denied(&self, tool_name: &str) -> bool {
        self.lock().denied.contains(tool_name)
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

    #[test]
    fn once_decisions_are_not_remembered() {
        let s = SessionApprovals::new();
        s.record("read_file", ApprovalDecision::ApprovedOnce);
        s.record("execute", ApprovalDecision::DeniedOnce);
        assert!(!s.is_allowed("read_file"));
        assert!(!s.is_denied("execute"));
    }

    #[test]
    fn session_decisions_are_remembered_per_tool() {
        let s = SessionApprovals::new();
        s.record("read_file", ApprovalDecision::ApprovedForSession);
        s.record("execute", ApprovalDecision::AlwaysDeniedForSession);
        assert!(s.is_allowed("read_file"));
        assert!(!s.is_allowed("execute"));
        assert!(s.is_denied("execute"));
        assert!(!s.is_denied("read_file"));
    }

    #[test]
    fn a_later_opposite_session_decision_replaces_the_earlier_one() {
        let s = SessionApprovals::new();
        s.record("grep", ApprovalDecision::ApprovedForSession);
        s.record("grep", ApprovalDecision::AlwaysDeniedForSession);
        assert!(!s.is_allowed("grep"));
        assert!(s.is_denied("grep"));
    }

    #[test]
    fn clones_share_the_same_memory() {
        let a = SessionApprovals::new();
        let b = a.clone();
        a.record("glob", ApprovalDecision::ApprovedForSession);
        assert!(b.is_allowed("glob"));
    }
}
