//! Lifecycle hook system (harness spec FR-7, overall design §4.5).
//!
//! Hooks are either Rust implementations of [`Hook`] (for embedders) or
//! [`CommandHook`]s — shell commands configured in `config.toml` that
//! receive the phase's JSON payload on stdin and may print a replacement.

pub mod command;
pub mod registry;

pub use command::CommandHook;
pub use registry::{HookFailure, HookRegistry};

use std::time::Duration;

use arbe_core::HookError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPhase {
    BeforeContextAssembly,
    BeforeModelCall,
    AfterModelCall,
    BeforeToolExecute,
    AfterToolExecute,
    OnError,
    OnTurnComplete,
    /// The model has answered and the turn is about to end, after it changed
    /// something. A hook can send the turn back to the model with feedback
    /// (a check that failed, or `"continue": "<what to do>"`).
    BeforeTurnEnd,
    /// A tool call is waiting for the user's approval. Notification only:
    /// what the hook returns is ignored.
    OnApprovalRequested,
}

impl HookPhase {
    /// The phase's config name (`before_tool_execute`, ...).
    pub fn name(self) -> &'static str {
        match self {
            Self::BeforeContextAssembly => "before_context_assembly",
            Self::BeforeModelCall => "before_model_call",
            Self::AfterModelCall => "after_model_call",
            Self::BeforeToolExecute => "before_tool_execute",
            Self::AfterToolExecute => "after_tool_execute",
            Self::OnError => "on_error",
            Self::OnTurnComplete => "on_turn_complete",
            Self::BeforeTurnEnd => "before_turn_end",
            Self::OnApprovalRequested => "on_approval_requested",
        }
    }

    pub const ALL: [HookPhase; 9] = [
        Self::BeforeContextAssembly,
        Self::BeforeModelCall,
        Self::AfterModelCall,
        Self::BeforeToolExecute,
        Self::AfterToolExecute,
        Self::OnError,
        Self::OnTurnComplete,
        Self::BeforeTurnEnd,
        Self::OnApprovalRequested,
    ];

    /// Parses a config name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn phase(&self) -> HookPhase;
    /// Returns the payload, changed or not. What a change means depends on
    /// the phase (see the runtime's hook payloads).
    async fn run(&self, payload: Value) -> Result<Value, HookError>;

    /// A name for failure reports.
    fn name(&self) -> String {
        format!("{} hook", self.phase().name())
    }

    /// This hook's own time limit; `None` uses the registry's default.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// Whether this hook's failure should block what it guards rather than
    /// be skipped (see [`HookFailure::blocking`]).
    fn blocks_on_failure(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_names_round_trip() {
        for phase in HookPhase::ALL {
            assert_eq!(HookPhase::from_name(phase.name()), Some(phase));
        }
        assert_eq!(HookPhase::from_name("before_everything"), None);
    }
}
