//! Domain types, event model, and error taxonomy shared across all
//! ArBeHarness crates. See `docs/v1-overall-design.md` and
//! `docs/v1-harness-spec.md` for the specification this crate implements.

pub mod error;
pub mod event;
pub mod ids;
pub mod loop_state;
pub mod message;
pub mod session;
pub mod tool;
pub mod turn;

pub use error::{ConfigError, HarnessError, HookError, MemoryError, ProviderError, ToolError};
pub use event::{RuntimeCommand, RuntimeEvent};
pub use ids::{SessionId, ToolCallId, TurnId};
pub use loop_state::{IllegalTransition, LoopMachine, LoopPhase};
pub use message::{Message, Role};
pub use session::{SessionMeta, SessionStatus};
pub use tool::{ApprovalDecision, ApprovalPolicyMode, RiskLevel, ToolInvocation, ToolResult};
pub use turn::Turn;
