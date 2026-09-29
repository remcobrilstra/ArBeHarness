//! Tool registry, approval policy, and execution contracts (harness spec
//! FR-4, overall design §4.6).

pub mod builtin;
pub mod gate;
pub mod policy;
pub mod registry;
pub mod rules;
pub mod session_approvals;

pub use gate::{Authorization, Authorized, GatedOutcome, authorize, execute_gated};
pub use policy::{PolicyOutcome, StandardApprovalPolicy};
pub use registry::ToolRegistry;
pub use rules::ToolRule;
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
    /// Calls that may run without asking (see `StandardApprovalPolicy`).
    pub allowlist: Vec<ToolRule>,
    /// Calls that are always refused.
    pub denylist: Vec<ToolRule>,
    /// Session-scoped human decisions, recorded by `execute_gated`.
    pub session: SessionApprovals,
    /// Whether an "approve for session" answer covers a `RiskLevel::High`
    /// tool as a whole. Off by default: such an answer then approves only
    /// that exact call (same command line), since approving `execute`
    /// itself would approve every later shell command.
    pub session_approval_covers_high_risk: bool,
}

impl ApprovalContext {
    /// A context with empty session memory and the safe high-risk default.
    /// Rules that don't parse (config validates them first) are taken as
    /// bare tool names rather than dropped.
    pub fn new(
        policy_mode: ApprovalPolicyMode,
        allowlist: Vec<String>,
        denylist: Vec<String>,
    ) -> Self {
        let parse = |rules: Vec<String>| -> Vec<ToolRule> {
            rules
                .iter()
                .map(|r| ToolRule::parse(r).unwrap_or_else(|_| ToolRule::exact(r, None)))
                .collect()
        };
        Self {
            policy_mode,
            allowlist: parse(allowlist),
            denylist: parse(denylist),
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
    /// `subject` is what the call acts on, from `ToolExecutor::subject`.
    fn decide(
        &self,
        invocation: &ToolInvocation,
        subject: Option<&str>,
        ctx: &ApprovalContext,
    ) -> PolicyOutcome;
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

/// What the caller of an approved tool call supplies for running it (see
/// [`Authorized::execute`]).
#[derive(Clone, Default)]
pub struct ToolRun {
    pub cancel: CancellationToken,
    pub progress: Option<ProgressSink>,
}

/// Everything a tool execution needs besides its invocation — and proof
/// that the call passed the approval gate: only the gate can create one
/// (a private field; no `Default` or `Clone`), and
/// [`ToolExecutor::execute`] requires one, so no code path can run a tool
/// without approval by accident. Unit tests of a single executor use
/// [`ToolContext::for_testing`] (the `testing` feature).
///
/// Outside this crate, a context can't be built by hand...
///
/// ```compile_fail
/// let ctx = arbe_tools::ToolContext {
///     cancel: arbe_tools::CancellationToken::new(),
///     progress: None,
/// };
/// ```
///
/// ...nor defaulted:
///
/// ```compile_fail
/// let ctx = arbe_tools::ToolContext::default();
/// ```
pub struct ToolContext {
    /// Fires when the turn is cancelled. Long-running tools should stop
    /// promptly (e.g. `execute` kills its child process) and return
    /// `ToolError::Cancelled`; quick tools may ignore it.
    pub cancel: CancellationToken,
    pub progress: Option<ProgressSink>,
    _issued_by_gate: IssuedByGate,
}

/// Only constructible in this crate.
struct IssuedByGate;

impl ToolContext {
    /// Issued by the gate for an approved call.
    pub(crate) fn issue(run: ToolRun) -> Self {
        Self {
            cancel: run.cancel,
            progress: run.progress,
            _issued_by_gate: IssuedByGate,
        }
    }

    /// A context for calling one executor directly in its own unit tests,
    /// bypassing approval. Never use it in harness code.
    #[cfg(any(test, feature = "testing"))]
    pub fn for_testing() -> Self {
        Self::issue(ToolRun::default())
    }

    /// [`for_testing`](Self::for_testing) with a given cancel token and
    /// progress sink.
    #[cfg(any(test, feature = "testing"))]
    pub fn for_testing_with(run: ToolRun) -> Self {
        Self::issue(run)
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

    /// Whether a call needs a human's approval when the policy would ask
    /// for one. `false` only for tools that act on nothing — that just talk
    /// to the user or read the harness's own state (e.g. `ask_user`):
    /// asking permission to ask a question helps no one. Such calls still
    /// go through the gate, so deny rules and dry-run mode still apply.
    fn requires_approval(&self) -> bool {
        true
    }

    /// Whether the tool leaves everything as it was: it reads files or the
    /// web, or talks to the user, but writes nothing, runs nothing and
    /// changes no shared state outside the session. Read-only session modes
    /// (plan mode) offer only these. Defaults to `false`, so a tool that
    /// doesn't say is treated as having side effects.
    fn read_only(&self) -> bool {
        false
    }

    /// The session this tool belongs to is ending: release anything that
    /// must not outlive it (e.g. `execute` stops its background
    /// processes). Called by [`ToolRegistry::close_all`].
    fn close(&self) {}

    /// What a call acts on, for permission rules like `tool(pattern)`: a
    /// path for file tools, the command line for `execute`. `None` (the
    /// default) means rules can only match this tool by name.
    fn subject(&self, _arguments: &Value) -> Option<String> {
        None
    }
}

/// A call's `path` argument as a rule subject: forward slashes, no
/// leading `./`, `default` when absent (e.g. `"."`).
pub fn path_subject(arguments: &Value, default: Option<&str>) -> Option<String> {
    let raw = arguments.get("path").and_then(Value::as_str).or(default)?;
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.strip_prefix("./").unwrap_or(&normalized);
    Some(if trimmed.is_empty() {
        ".".to_string()
    } else {
        trimmed.to_string()
    })
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
        self.execute(invocation, &ToolContext::for_testing()).await
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
