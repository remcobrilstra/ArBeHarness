use arbe_core::Message;
use serde::{Deserialize, Serialize};

/// A session-history message tagged with the turn it belongs to, so
/// strategies can honor `pinned_turn_indices` (harness spec FR-5: pinned
/// message retention) without guessing at message boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub turn_index: u64,
    pub message: Message,
}
