use std::collections::HashMap;
use std::sync::Mutex;

use arbe_core::{ApprovalDecision, ToolCallId};
use tokio::sync::oneshot;

/// Mailbox for human decisions on tool calls that are paused awaiting
/// approval mid-turn. The waiting turn registers a call and awaits the
/// receiver; `Agent::supply_tool_decision` (callable while the turn runs,
/// since `Agent`'s methods take `&self`) delivers the decision.
#[derive(Default)]
pub(super) struct ToolDecisions {
    waiting: Mutex<HashMap<ToolCallId, oneshot::Sender<ApprovalDecision>>>,
}

impl ToolDecisions {
    pub(super) fn register(&self, id: ToolCallId) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id, tx);
        rx
    }

    /// Delivers `decision` to the call waiting on `id`. `false` if nothing
    /// is (still) waiting on it — already decided, timed out, or cancelled.
    pub(super) fn supply(&self, id: ToolCallId, decision: ApprovalDecision) -> bool {
        match self.lock().remove(&id) {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Forgets a registration whose waiter gave up (turn cancelled).
    pub(super) fn withdraw(&self, id: ToolCallId) {
        self.lock().remove(&id);
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<ToolCallId, oneshot::Sender<ApprovalDecision>>> {
        self.waiting.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_supplied_decision_reaches_the_waiter_exactly_once() {
        let decisions = ToolDecisions::default();
        let id = ToolCallId::new();
        let rx = decisions.register(id);
        assert!(decisions.supply(id, ApprovalDecision::ApprovedOnce));
        assert_eq!(rx.await.unwrap(), ApprovalDecision::ApprovedOnce);
        assert!(!decisions.supply(id, ApprovalDecision::DeniedOnce));
    }

    #[test]
    fn withdrawn_or_unknown_ids_are_not_deliverable() {
        let decisions = ToolDecisions::default();
        let id = ToolCallId::new();
        let _rx = decisions.register(id);
        decisions.withdraw(id);
        assert!(!decisions.supply(id, ApprovalDecision::ApprovedOnce));
        assert!(!decisions.supply(ToolCallId::new(), ApprovalDecision::ApprovedOnce));
    }
}
