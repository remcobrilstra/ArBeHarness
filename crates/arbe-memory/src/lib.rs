//! Context assembly + memory strategy contracts (harness spec FR-5,
//! overall design §5). Truncation/compaction/pinning implementations land
//! in Phase 3; this crate currently defines only the shared contract.

use arbe_core::Message;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInput {
    pub system_instructions: Vec<String>,
    pub session_history: Vec<Message>,
    pub memory_notes: Vec<String>,
    pub budget_tokens: u64,
    pub pinned_turn_indices: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOutput {
    pub messages: Vec<Message>,
    pub estimated_tokens: u64,
    pub truncated: bool,
}

pub trait ContextStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn build_context(&self, input: ContextInput) -> ContextOutput;
}
