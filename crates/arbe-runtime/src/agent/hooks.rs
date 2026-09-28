//! Typed payloads for each lifecycle hook phase (harness spec FR-7).
//!
//! Hooks exchange JSON, but the shapes are defined here once rather than
//! assembled ad hoc at each call site, so a hook author has one place to
//! read what each phase sends.

use arbe_hooks::{HookPhase, HookRegistry};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct TurnPayload {
    pub turn_id: String,
}

#[derive(Debug, Serialize)]
pub(super) struct ModelCallPayload {
    pub turn_id: String,
    pub round: u32,
    pub message_count: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct ModelResultPayload {
    pub turn_id: String,
    pub round: u32,
    pub text_chars: usize,
    pub tool_calls: usize,
}

/// `BeforeToolExecute` runs *before* the approval gate, so a human always
/// approves the arguments that will actually run. A hook may return:
/// - the payload with `arguments` changed, to rewrite the call, or
/// - the payload plus `"veto": "<reason>"`, to deny it.
#[derive(Debug, Serialize)]
pub(super) struct ToolCallPayload<'a> {
    pub turn_id: String,
    pub tool_name: &'a str,
    pub arguments: &'a Value,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolResultPayload<'a> {
    pub turn_id: String,
    pub tool_name: &'a str,
    pub is_error: bool,
    pub output_chars: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct ErrorPayload {
    pub turn_id: String,
    pub error: String,
}

/// Runs `phase`'s hooks over `payload` and returns the (possibly
/// transformed) result. Hooks are isolated by `HookRegistry` (timeouts,
/// panics), so this never fails.
pub(super) async fn run(hooks: &HookRegistry, phase: HookPhase, payload: &impl Serialize) -> Value {
    let value = serde_json::to_value(payload).unwrap_or(Value::Null);
    hooks.run_phase(phase, value).await
}

/// How a `BeforeToolExecute` result changes the call.
#[derive(Debug, PartialEq)]
pub(super) enum ToolCallVerdict {
    Proceed(Value),
    Veto(String),
}

/// Reads a `BeforeToolExecute` result: a `veto` string denies the call;
/// otherwise the (possibly rewritten) `arguments` are used, falling back to
/// the originals if a hook dropped them.
pub(super) fn tool_call_verdict(result: &Value, original_arguments: &Value) -> ToolCallVerdict {
    if let Some(reason) = result.get("veto").and_then(Value::as_str) {
        return ToolCallVerdict::Veto(reason.to_string());
    }
    ToolCallVerdict::Proceed(
        result
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| original_arguments.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn verdict_reads_vetoes_and_rewrites() {
        let original = json!({"path": "a"});
        assert_eq!(
            tool_call_verdict(&json!({"arguments": {"path": "a"}}), &original),
            ToolCallVerdict::Proceed(original.clone())
        );
        assert_eq!(
            tool_call_verdict(&json!({"arguments": {"path": "b"}}), &original),
            ToolCallVerdict::Proceed(json!({"path": "b"}))
        );
        assert_eq!(
            tool_call_verdict(&json!({"veto": "no writes on Fridays"}), &original),
            ToolCallVerdict::Veto("no writes on Fridays".into())
        );
        // A hook that returns something unexpected doesn't lose the call.
        assert_eq!(
            tool_call_verdict(&Value::Null, &original),
            ToolCallVerdict::Proceed(original.clone())
        );
    }
}
