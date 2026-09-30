use std::collections::HashMap;
use std::sync::Mutex;

use arbe_core::{ApprovalDecision, RiskLevel, ToolCallId, TurnId};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

/// A tool call waiting for a human decision — what the
/// `ToolCallProposed`/`ToolApprovalRequested` events said about it, kept so
/// a UI that missed them (fell behind, reconnected) can still ask.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PendingDecision {
    pub tool_call_id: ToolCallId,
    pub turn_id: TurnId,
    pub tool_name: String,
    pub arguments: Value,
    pub risk: RiskLevel,
}

/// Mailbox for human decisions on tool calls that are paused awaiting
/// approval mid-turn. The waiting turn registers a call and awaits the
/// receiver; `Agent::supply_tool_decision` (callable while the turn runs,
/// since `Agent`'s methods take `&self`) delivers the decision.
#[derive(Default)]
pub(super) struct ToolDecisions {
    waiting: Mutex<HashMap<ToolCallId, Waiting>>,
}

struct Waiting {
    reply: oneshot::Sender<ApprovalDecision>,
    call: PendingDecision,
}

impl ToolDecisions {
    pub(super) fn register(&self, call: PendingDecision) -> oneshot::Receiver<ApprovalDecision> {
        let (reply, rx) = oneshot::channel();
        self.lock()
            .insert(call.tool_call_id, Waiting { reply, call });
        rx
    }

    /// Delivers `decision` to the call waiting on `id`. `false` if nothing
    /// is (still) waiting on it — already decided, timed out, or cancelled.
    pub(super) fn supply(&self, id: ToolCallId, decision: ApprovalDecision) -> bool {
        match self.lock().remove(&id) {
            Some(waiting) => waiting.reply.send(decision).is_ok(),
            None => false,
        }
    }

    /// Forgets a registration whose waiter gave up (turn cancelled).
    pub(super) fn withdraw(&self, id: ToolCallId) {
        self.lock().remove(&id);
    }

    /// Every call waiting for a decision.
    pub(super) fn pending(&self) -> Vec<PendingDecision> {
        self.lock().values().map(|w| w.call.clone()).collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ToolCallId, Waiting>> {
        self.waiting.lock().unwrap_or_else(|p| p.into_inner())
    }
}
