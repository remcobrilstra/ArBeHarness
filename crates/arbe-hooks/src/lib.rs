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
        }
    }

    pub const ALL: [HookPhase; 7] = [
        Self::BeforeContextAssembly,
        Self::BeforeModelCall,
        Self::AfterModelCall,
        Self::BeforeToolExecute,
        Self::AfterToolExecute,
        Self::OnError,
        Self::OnTurnComplete,
    ];

    /// Parses a config name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn phase(&self) -> HookPhase;
    /// Read-only hooks return the payload unchanged; transforming hooks
    /// must be explicitly permitted by policy (overall design §4.5).
    async fn run(&self, payload: Value) -> Result<Value, HookError>;

    /// A name for failure reports.
    fn name(&self) -> String {
        format!("{} hook", self.phase().name())
    }

    /// This hook's own time limit; `None` uses the registry's default.
    fn timeout(&self) -> Option<Duration> {
        None
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
