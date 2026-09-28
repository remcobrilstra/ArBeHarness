//! Context assembly + memory strategy contracts (harness spec FR-5,
//! overall design §5). Ships the two required v1 history-selection
//! strategies (truncation, compact-with-summary — both honor pinned
//! turns) plus the pipeline that assembles a full context from
//! instructions/skills/memory/history/user-input (overall design §5.2).

pub mod compact_summary;
pub mod history;
pub mod pipeline;
pub mod prune;
pub mod tokens;
pub mod truncation;

pub use compact_summary::CompactWithSummaryStrategy;
pub use history::HistoryEntry;
pub use pipeline::ContextPipeline;
pub use prune::{drop_oldest_turns, prune_history, prune_tool_results};
pub use tokens::{TokenCalibration, estimate_message_tokens, estimate_tokens};
pub use truncation::TruncationStrategy;

use serde::{Deserialize, Serialize};

/// Borrows the caller's session history/pinned-index storage rather than
/// taking ownership, so assembling context for a turn doesn't require
/// cloning the entire session history just to hand it to a strategy that
/// only ever reads it.
#[derive(Debug, Clone)]
pub struct ContextInput<'a> {
    pub session_history: &'a [HistoryEntry],
    pub budget_tokens: u64,
    pub pinned_turn_indices: &'a [u64],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    pub messages: Vec<arbe_core::Message>,
    pub estimated_tokens: u64,
    pub truncated: bool,
    /// How many old tool results were stubbed out to fit the budget
    /// before any history had to be dropped.
    #[serde(default)]
    pub pruned_tool_results: usize,
}

/// Selects/budgets session history. Config-selectable per overall design §7
/// ("memory.strategy"); switchable at runtime without a code change.
pub trait ContextStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn build_context(&self, input: ContextInput<'_>) -> ContextOutput;
}
