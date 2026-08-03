use serde::{Deserialize, Serialize};

/// Explicit agent loop states (overall design §4.1, harness spec FR-3).
/// The runtime drives transitions through these phases for every turn;
/// each transition must emit a `RuntimeEvent` (see `event.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopPhase {
    ReceiveUserInput,
    AssembleContext,
    PlanOrDirectRespond,
    ModelInference,
    InterpretOutput,
    ToolApproval,
    ToolExecution,
    PostToolReflection,
    PersistTurn,
    EmitEvents,
    Idle,
}

impl LoopPhase {
    /// The phase a fresh, idle loop starts from once a user message arrives.
    pub fn initial() -> Self {
        Self::Idle
    }
}
