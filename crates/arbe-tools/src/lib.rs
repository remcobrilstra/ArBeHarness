//! Tool registry, approval policy, and execution contracts (harness spec
//! FR-4, overall design §4.6).

pub mod builtin;
pub mod gate;
pub mod policy;
pub mod registry;
pub mod session_approvals;

pub use gate::{Authorization, Authorized, GatedOutcome, authorize, execute_gated};
pub use policy::{PolicyOutcome, StandardApprovalPolicy};
pub use registry::ToolRegistry;
pub use schemars;
pub use session_approvals::SessionApprovals;

use std::sync::Arc;

use arbe_core::{ApprovalPolicyMode, RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
pub use tokio_util::sync::CancellationToken;

/// The policy config in effect for a decision (harness spec FR-4). Kept
/// separate from `StandardApprovalPolicy` itself so the same policy
/// implementation can be reused across sessions/profiles that configure
/// different modes/lists.
pub struct ApprovalContext {
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
    /// Session-scoped human decisions, recorded by `execute_gated`.
    pub session: SessionApprovals,
    /// Whether an "approve for session" answer also covers
    /// `RiskLevel::High` tools. Off by default: approving `execute` for the
    /// session would otherwise auto-approve *every* later shell command,
    /// whatever it is, so high-risk calls keep prompting each time unless
    /// config explicitly opts in.
    pub session_approval_covers_high_risk: bool,
}

impl ApprovalContext {
    /// A context with empty session memory and the safe high-risk default.
    pub fn new(
        policy_mode: ApprovalPolicyMode,
        allowlist: Vec<String>,
        denylist: Vec<String>,
    ) -> Self {
        Self {
            policy_mode,
            allowlist,
            denylist,
            session: SessionApprovals::new(),
            session_approval_covers_high_risk: false,
        }
    }
}

/// Decides, without asking a human, what should happen to an invocation.
/// `RequiresPrompt` means the runtime must pause and route the decision to
/// a human (TUI-FR-2) — it is not itself a final answer, unlike
/// `arbe_core::ApprovalDecision` which *is* a human's final answer.
pub trait ApprovalPolicy: Send + Sync {
    fn decide(&self, invocation: &ToolInvocation, ctx: &ApprovalContext) -> PolicyOutcome;
}

/// What the model is told about a tool: a description and a JSON Schema
/// for its arguments. The tool's *name* is whatever it's registered under.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescription {
    pub description: String,
    pub parameters: Value,
}

impl ToolDescription {
    /// Derives the argument schema from the type the tool actually parses
    /// its arguments into, so the two can't drift apart. Field doc comments
    /// become the per-argument descriptions the model sees. Subschemas are
    /// inlined (no `$ref`), which small local models handle far better.
    pub fn from_args<T: schemars::JsonSchema>(description: impl Into<String>) -> Self {
        let mut schema = schemars::generate::SchemaSettings::draft07()
            .with(|s| s.inline_subschemas = true)
            .into_generator()
            .into_root_schema_for::<T>();
        schema.remove("$schema");
        schema.remove("title");
        let mut parameters = schema.to_value();
        simplify_schema(&mut parameters);
        Self {
            description: description.into(),
            parameters,
        }
    }

    /// Accepts any arguments; for tools that don't describe themselves.
    pub fn untyped(description: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }
}

/// Makes a generated schema friendlier to models and strict providers:
/// Rust-specific `format`s (`uint64`, `int32`, `double`, ...) aren't JSON
/// Schema formats and some APIs reject them; and `"type": [T, "null"]`
/// (from `Option<T>`) invites models to send an explicit `null` where an
/// optional argument should simply be left out — optionality is already
/// expressed by `required`.
fn simplify_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(format)) = map.get("format")
                && (format.starts_with("uint")
                    || format.starts_with("int")
                    || format == "double"
                    || format == "float")
            {
                map.remove("format");
            }
            if let Some(Value::Array(types)) = map.get("type") {
                let non_null: Vec<Value> = types
                    .iter()
                    .filter(|t| t.as_str() != Some("null"))
                    .cloned()
                    .collect();
                if non_null.len() == 1 && non_null.len() < types.len() {
                    map.insert("type".to_string(), non_null[0].clone());
                }
            }
            map.values_mut().for_each(simplify_schema);
        }
        Value::Array(items) => items.iter_mut().for_each(simplify_schema),
        _ => {}
    }
}

/// Receives human-readable progress updates from a running tool (e.g. a
/// long command's latest output line), for UIs to show while it runs.
pub type ProgressSink = Arc<dyn Fn(String) + Send + Sync>;

/// Everything a tool execution needs besides its invocation.
#[derive(Clone, Default)]
pub struct ToolContext {
    /// Fires when the turn is cancelled. Long-running tools should stop
    /// promptly (e.g. `execute` kills its child process) and return
    /// `ToolError::Cancelled`; quick tools may ignore it.
    pub cancel: CancellationToken,
    pub progress: Option<ProgressSink>,
}

impl ToolContext {
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            progress: None,
        }
    }

    /// Reports progress, if anyone is listening.
    pub fn report(&self, update: impl Into<String>) {
        if let Some(progress) = &self.progress {
            progress(update.into());
        }
    }
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;

    /// Whether this tool may run concurrently with other calls from the
    /// same model response. Tools with side effects that could conflict
    /// (writing files, running commands, replacing shared state) return
    /// `false` and run on their own, in order.
    fn parallel_safe(&self) -> bool {
        true
    }

    /// What the model is told about this tool. The default accepts any
    /// arguments and has no description — every real tool overrides it.
    fn description(&self) -> ToolDescription {
        ToolDescription::untyped("")
    }

    /// The risk level shown when a call needs approval, and used by the
    /// policy (a `High` call never auto-approves). Read-only tools are
    /// `Low`, local mutation `Medium`, arbitrary execution `High`.
    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }
}

/// Test-only shorthand: run a tool with a default (never-cancelled)
/// context, so tool unit tests don't each construct one.
#[cfg(test)]
pub(crate) trait ExecuteWithDefaultContext {
    async fn execute_default(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError>;
}

#[cfg(test)]
impl<T: ToolExecutor + ?Sized> ExecuteWithDefaultContext for T {
    async fn execute_default(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        self.execute(invocation, &ToolContext::default()).await
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simplify_schema_drops_rust_formats_and_nullable_types_recursively() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "n": {"type": ["integer", "null"], "format": "uint64", "minimum": 0},
                "when": {"type": "string", "format": "date-time"},
                "nested": {"type": "array", "items": {"type": ["number", "null"], "format": "double"}}
            }
        });
        simplify_schema(&mut schema);
        assert_eq!(
            schema["properties"]["n"],
            json!({"type": "integer", "minimum": 0})
        );
        // Real JSON Schema formats are kept.
        assert_eq!(schema["properties"]["when"]["format"], "date-time");
        assert_eq!(
            schema["properties"]["nested"]["items"],
            json!({"type": "number"})
        );
    }
}
