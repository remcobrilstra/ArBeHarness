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

    /// Whether `to` is a legal next phase from `self`. Encodes the branches
    /// in overall design §4.1: `InterpretOutput` may skip straight to
    /// `PersistTurn` when no tool call was produced, tool calls may be
    /// denied (skipping `ToolExecution`), and follow-up inference loops
    /// `PostToolReflection` back to `ModelInference`.
    pub fn can_transition_to(self, to: LoopPhase) -> bool {
        use LoopPhase::*;
        matches!(
            (self, to),
            (Idle, ReceiveUserInput)
                | (ReceiveUserInput, AssembleContext)
                | (AssembleContext, PlanOrDirectRespond)
                | (PlanOrDirectRespond, ModelInference)
                | (ModelInference, InterpretOutput)
                | (InterpretOutput, ToolApproval)
                | (InterpretOutput, PersistTurn)
                | (ToolApproval, ToolExecution)
                | (ToolApproval, PersistTurn)
                | (ToolExecution, PostToolReflection)
                | (PostToolReflection, ModelInference)
                | (PostToolReflection, PersistTurn)
                | (PersistTurn, EmitEvents)
                | (EmitEvents, Idle)
        )
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("illegal loop transition: {from:?} -> {to:?}")]
pub struct IllegalTransition {
    pub from: LoopPhase,
    pub to: LoopPhase,
}

/// Tracks the current phase of one turn's pass through the agent loop and
/// rejects illegal transitions instead of silently allowing the runtime to
/// skip a required step (e.g. tool execution without approval).
#[derive(Debug, Clone, Copy)]
pub struct LoopMachine {
    current: LoopPhase,
}

impl LoopMachine {
    pub fn new() -> Self {
        Self {
            current: LoopPhase::initial(),
        }
    }

    pub fn current(&self) -> LoopPhase {
        self.current
    }

    pub fn transition(&mut self, to: LoopPhase) -> Result<LoopPhase, IllegalTransition> {
        if self.current.can_transition_to(to) {
            self.current = to;
            Ok(self.current)
        } else {
            Err(IllegalTransition {
                from: self.current,
                to,
            })
        }
    }
}

impl Default for LoopMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LoopPhase::*;

    #[test]
    fn direct_response_path_skips_tool_phases() {
        let mut m = LoopMachine::new();
        for phase in [
            ReceiveUserInput,
            AssembleContext,
            PlanOrDirectRespond,
            ModelInference,
            InterpretOutput,
            PersistTurn,
            EmitEvents,
            Idle,
        ] {
            m.transition(phase).unwrap();
        }
        assert_eq!(m.current(), Idle);
    }

    #[test]
    fn tool_call_path_with_followup_inference() {
        let mut m = LoopMachine::new();
        for phase in [
            ReceiveUserInput,
            AssembleContext,
            PlanOrDirectRespond,
            ModelInference,
            InterpretOutput,
            ToolApproval,
            ToolExecution,
            PostToolReflection,
            ModelInference,
            InterpretOutput,
            PersistTurn,
            EmitEvents,
            Idle,
        ] {
            m.transition(phase).unwrap();
        }
        assert_eq!(m.current(), Idle);
    }

    #[test]
    fn denied_tool_call_skips_execution() {
        let mut m = LoopMachine::new();
        for phase in [
            ReceiveUserInput,
            AssembleContext,
            PlanOrDirectRespond,
            ModelInference,
            InterpretOutput,
            ToolApproval,
            PersistTurn,
        ] {
            m.transition(phase).unwrap();
        }
        assert_eq!(m.current(), PersistTurn);
    }

    #[test]
    fn cannot_skip_approval_to_execute_a_tool() {
        let mut m = LoopMachine::new();
        for phase in [
            ReceiveUserInput,
            AssembleContext,
            PlanOrDirectRespond,
            ModelInference,
            InterpretOutput,
        ] {
            m.transition(phase).unwrap();
        }
        let err = m.transition(ToolExecution).unwrap_err();
        assert_eq!(err.from, InterpretOutput);
        assert_eq!(err.to, ToolExecution);
        // A rejected transition must not move current().
        assert_eq!(m.current(), InterpretOutput);
    }
}
