//! Lifecycle hook system (harness spec FR-7, overall design §4.5).

pub mod registry;

pub use registry::HookRegistry;

use arbe_core::HookError;
use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPhase {
    BeforeContextAssembly,
    BeforeModelCall,
    AfterModelCall,
    BeforeToolExecute,
    AfterToolExecute,
    OnError,
    OnTurnComplete,
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn phase(&self) -> HookPhase;
    /// Read-only hooks return the payload unchanged; transforming hooks
    /// must be explicitly permitted by policy (overall design §4.5).
    async fn run(&self, payload: Value) -> Result<Value, HookError>;
}
