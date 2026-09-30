//! Domain types, event model, and error taxonomy shared across all
//! ArBeHarness crates. See `docs/v1-overall-design.md` and
//! `docs/v1-harness-spec.md` for the specification this crate implements.

pub mod context;
pub mod error;
pub mod event;
pub mod ids;
pub mod loop_state;
pub mod message;
pub mod session;
pub mod shell;
pub mod tool;
pub mod turn;
pub mod usage;

pub use context::{ContextBreakdown, ContextUsage, MessageTokens};
pub use error::{
    ConfigError, HarnessError, HookError, MemoryError, ProviderError, ToolError, UserFacing,
};
pub use event::{EventEnvelope, RuntimeEvent};
pub use ids::{SessionId, ToolCallId, TurnId};
pub use loop_state::{IllegalTransition, LoopMachine, LoopPhase};
pub use message::{ContentBlock, ImageSource, Message, Role};
pub use session::{SessionActivity, SessionMeta, SessionStatus};
pub use tool::{
    ApprovalDecision, ApprovalPolicyMode, RequestedToolCall, RiskLevel, ToolInvocation, ToolResult,
    ToolSpec,
};
pub use turn::{Compaction, TURN_SCHEMA_VERSION, Turn};
pub use usage::{Pricing, StopReason, Usage};
