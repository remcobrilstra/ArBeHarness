use arbe_core::{StopReason, Usage};
use serde_json::Value;

/// One typed event from a provider's streaming response. Every adapter
/// translates its wire format (OpenAI SSE, Ollama NDJSON, Anthropic SSE
/// events, ...) into this one vocabulary, so the runtime and
/// [`crate::ResponseAccumulator`] never see provider specifics.
///
/// Ordering contract adapters must honor:
/// - `ToolUseStart { id }` comes before any `ToolUseInputDelta { id }`, and
///   `ToolUseEnd { id }` (if sent) after the last one. Tool uses may
///   interleave with each other (OpenAI streams several at once by index).
/// - `Usage` may be sent more than once; each carries the latest *known*
///   cumulative counts (a field left 0 means "not reported in this event",
///   not "zero so far").
/// - `Stop` is sent at most once, but not necessarily last (OpenAI reports
///   usage in a chunk *after* the finish reason).
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// Integrity signature for the current thinking block (Anthropic).
    ThinkingSignature(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    /// A fragment of the tool call's JSON arguments. Fragments concatenate
    /// into the full JSON text; individual fragments needn't be valid JSON.
    ToolUseInputDelta {
        id: String,
        partial_json: String,
    },
    ToolUseEnd {
        id: String,
    },
    /// A provider-specific content block to round-trip verbatim.
    Opaque {
        provider: String,
        data: Value,
    },
    Usage(Usage),
    Stop(StopReason),
}
