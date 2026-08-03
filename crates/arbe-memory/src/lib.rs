//! Context assembly + memory strategy contracts (harness spec FR-5,
//! overall design §5). Ships the two required v1 history-selection
//! strategies (truncation, compact-with-summary — both honor pinned
//! turns) plus the pipeline that assembles a full context from
//! instructions/skills/memory/history/user-input (overall design §5.2).

pub mod compact_summary;
pub mod history;
pub mod pipeline;
pub mod tokens;
pub mod truncation;

pub use compact_summary::CompactWithSummaryStrategy;
pub use history::HistoryEntry;
pub use pipeline::ContextPipeline;
pub use tokens::estimate_tokens;
pub use truncation::TruncationStrategy;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInput {
    pub session_history: Vec<HistoryEntry>,
    pub budget_tokens: u64,
    pub pinned_turn_indices: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    pub messages: Vec<arbe_core::Message>,
    pub estimated_tokens: u64,
    pub truncated: bool,
}

/// Selects/budgets session history. Config-selectable per overall design §7
/// ("memory.strategy"); switchable at runtime without a code change.
pub trait ContextStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn build_context(&self, input: ContextInput) -> ContextOutput;
}
