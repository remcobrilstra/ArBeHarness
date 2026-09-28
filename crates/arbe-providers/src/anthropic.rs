//! Anthropic Messages API adapter (`POST /v1/messages`, streaming).
//!
//! Differences from the OpenAI shape that this module absorbs:
//! - system instructions are a separate `system` parameter, so
//!   `Role::System` messages are hoisted out of the conversation;
//! - tool results are `tool_result` blocks inside a *user* message, and
//!   roles must alternate, so consecutive same-role messages are merged;
//! - thinking blocks carry a signature that must be echoed back verbatim;
//!   redacted thinking arrives as an opaque block and is round-tripped;
//! - prompt caching is explicit (`cache_control` breakpoints).

use std::collections::HashMap;

use arbe_core::{ContentBlock, ImageSource, Message, ProviderError, Role, StopReason, Usage};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::catalog::ModelCatalog;
use crate::sse::{SseDecoder, SseItem};
use crate::{ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent, ProviderStream, http};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// Tag for `ContentBlock::Opaque` blocks this adapter produced (and is the
/// only one to send back).
const PROVIDER_ID: &str = "anthropic";

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    extra_headers: Vec<(String, String)>,
    catalog: ModelCatalog,
}

impl AnthropicProvider {
    /// `api_key` should come from env/config indirection per NFR-4 — never
    /// hardcode or log it.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: http::client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            extra_headers: Vec::new(),
            catalog: ModelCatalog::new(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Headers sent on every request (e.g. `anthropic-beta` flags).
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn with_catalog(mut self, catalog: ModelCatalog) -> Self {
        self.catalog = catalog;
        self
    }
}

// ---------------------------------------------------------------------------
// Request mapping
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ApiRequest {
    model: String,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<Value>,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    /// Omitted with extended thinking, which requires the default (1).
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    stream: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ApiMessage {
    role: &'static str,
    content: Vec<Value>,
}

fn cache_control() -> Value {
    json!({ "type": "ephemeral" })
}

fn image_block(source: &ImageSource, media_type: &str) -> Value {
    match source {
        ImageSource::Base64 { data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }),
        ImageSource::Url { url } => json!({
            "type": "image",
            "source": { "type": "url", "url": url },
        }),
    }
}

/// Maps one harness block to Anthropic's shape, or `None` for blocks that
/// can't be sent (empty text, thinking from another provider, other
/// providers' opaque blocks).
fn api_block(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text { text } if text.is_empty() => None,
        ContentBlock::Text { text } => Some(json!({ "type": "text", "text": text })),
        ContentBlock::Image { source, media_type } => Some(image_block(source, media_type)),
        ContentBlock::ToolUse { id, name, input } => Some(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            // The API requires an object; a model's malformed arguments
            // (kept as a raw string, see `parse_tool_input`) are wrapped.
            "input": if input.is_object() { input.clone() } else { json!({ "raw_arguments": input }) },
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let content: Vec<Value> = content.iter().filter_map(api_block).collect();
            let mut block = json!({ "type": "tool_result", "tool_use_id": tool_use_id });
            if !content.is_empty() {
                block["content"] = Value::Array(content);
            }
            if *is_error {
                block["is_error"] = Value::Bool(true);
            }
            Some(block)
        }
        // Only signed thinking can be sent back, and only Anthropic signs.
        ContentBlock::Thinking {
            text,
            signature: Some(signature),
        } => Some(json!({ "type": "thinking", "thinking": text, "signature": signature })),
        ContentBlock::Thinking {
            signature: None, ..
        } => None,
        ContentBlock::Opaque { provider, data } if provider == PROVIDER_ID => Some(data.clone()),
        ContentBlock::Opaque { .. } => None,
    }
}

/// Splits the conversation into Anthropic's `system` and `messages`.
///
/// System messages are hoisted, in order, into `system` (so a memory note
/// placed late in the harness context loses its position but not its
/// content). Tool messages become user messages; consecutive same-role
/// messages are merged, since roles must alternate; tool results are moved
/// to the front of their user message, as the API requires. Cache
/// breakpoints go on the last system block (caching tools + system), on
/// any message flagged `cache_breakpoint`, and on the final message (so the
/// next request in a tool loop reads the whole conversation from cache).
fn split_conversation(messages: &[Message]) -> (Vec<Value>, Vec<ApiMessage>) {
    let mut system = Vec::new();
    let mut out: Vec<(ApiMessage, bool)> = Vec::new();

    for message in messages {
        if message.role == Role::System {
            system.extend(message.content.iter().filter_map(api_block));
            continue;
        }
        let role = match message.role {
            Role::Assistant => "assistant",
            _ => "user",
        };
        let blocks: Vec<Value> = message.content.iter().filter_map(api_block).collect();
        if blocks.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some((last, breakpoint)) if last.role == role => {
                last.content.extend(blocks);
                *breakpoint |= message.cache_breakpoint;
            }
            _ => out.push((
                ApiMessage {
                    role,
                    content: blocks,
                },
                message.cache_breakpoint,
            )),
        }
    }

    // The conversation must open with a user turn; history trimming can
    // leave an assistant message first.
    if out.first().is_some_and(|(m, _)| m.role == "assistant") {
        out.insert(
            0,
            (
                ApiMessage {
                    role: "user",
                    content: vec![
                        json!({ "type": "text", "text": "[earlier conversation omitted]" }),
                    ],
                },
                false,
            ),
        );
    }

    let last_index = out.len().saturating_sub(1);
    let messages = out
        .into_iter()
        .enumerate()
        .map(|(i, (mut message, breakpoint))| {
            if message.role == "user" {
                message.content.sort_by_key(|b| b["type"] != "tool_result");
            }
            if (breakpoint || i == last_index)
                && let Some(last) = message.content.last_mut()
            {
                last["cache_control"] = cache_control();
            }
            message
        })
        .collect();

    if let Some(last) = system.last_mut() {
        last["cache_control"] = cache_control();
    }
    (system, messages)
}

fn build_request_body(req: &ModelRequest) -> ApiRequest {
    let (system, messages) = split_conversation(&req.messages);
    let tools: Vec<Value> = req
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect();
    let tool_choice = (!tools.is_empty()).then(|| json!({ "type": "auto" }));

    // Thinking tokens count against `max_tokens`, which must exceed the
    // budget — so the budget is added on top of the requested output cap
    // rather than silently eating into it.
    let (max_tokens, thinking, temperature) = match req.thinking_budget_tokens {
        Some(budget) => (
            req.max_tokens + budget,
            Some(json!({ "type": "enabled", "budget_tokens": budget })),
            None,
        ),
        None => (req.max_tokens, None, Some(req.temperature)),
    };

    ApiRequest {
        model: req.model.clone(),
        max_tokens,
        system,
        messages,
        tools,
        tool_choice,
        temperature,
        thinking,
        stream: true,
    }
}

// ---------------------------------------------------------------------------
// Stream translation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent {
    MessageStart {
        message: StartMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: Value,
    },
    ContentBlockDelta {
        index: u32,
        delta: Value,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        #[serde(default)]
        delta: MessageDeltaBody,
        #[serde(default)]
        usage: Option<ApiUsage>,
    },
    MessageStop,
    Ping,
    Error {
        error: ApiError,
    },
    /// Event types added to the API later are ignored rather than fatal.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct StartMessage {
    #[serde(default)]
    usage: Option<ApiUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct MessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: String,
}

impl From<ApiUsage> for Usage {
    fn from(u: ApiUsage) -> Self {
        // Anthropic's input_tokens already excludes cache reads/writes,
        // matching `Usage::input_tokens`.
        Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_write_tokens: u.cache_creation_input_tokens,
        }
    }
}

/// Remembers which open content-block index is a tool use (and its id),
/// since deltas and stops only carry the index.
#[derive(Debug, Default)]
struct StreamState {
    tool_ids_by_index: HashMap<u32, String>,
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "refusal" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

fn map_stream_error(error: ApiError) -> ProviderError {
    let message = format!("{}: {}", error.kind, error.message);
    match error.kind.as_str() {
        "overloaded_error" => ProviderError::Overloaded(message),
        "rate_limit_error" => ProviderError::rate_limit(message),
        "authentication_error" | "permission_error" => ProviderError::Auth(message),
        "invalid_request_error" => ProviderError::InvalidRequest(message),
        _ => ProviderError::Internal(message),
    }
}

fn translate_event(
    state: &mut StreamState,
    payload: &str,
) -> Result<Vec<ProviderEvent>, ProviderError> {
    let event: StreamEvent = serde_json::from_str(payload).map_err(|e| {
        ProviderError::Internal(format!("failed to parse Anthropic stream event: {e}"))
    })?;
    let mut events = Vec::new();
    match event {
        StreamEvent::MessageStart { message } => {
            if let Some(usage) = message.usage {
                events.push(ProviderEvent::Usage(usage.into()));
            }
        }
        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => match content_block["type"].as_str().unwrap_or_default() {
            "text" => {
                let text = content_block["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    events.push(ProviderEvent::TextDelta(text.to_string()));
                }
            }
            "thinking" => {
                let text = content_block["thinking"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    events.push(ProviderEvent::ThinkingDelta(text.to_string()));
                }
            }
            "tool_use" => {
                let id = content_block["id"].as_str().unwrap_or_default().to_string();
                let name = content_block["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                state.tool_ids_by_index.insert(index, id.clone());
                events.push(ProviderEvent::ToolUseStart { id, name });
            }
            // Redacted thinking (and any block type added later) is kept
            // verbatim so it can be sent back unchanged.
            _ => events.push(ProviderEvent::Opaque {
                provider: PROVIDER_ID.to_string(),
                data: content_block,
            }),
        },
        StreamEvent::ContentBlockDelta { index, delta } => {
            let text = |key: &str| delta[key].as_str().unwrap_or_default().to_string();
            match delta["type"].as_str().unwrap_or_default() {
                "text_delta" => events.push(ProviderEvent::TextDelta(text("text"))),
                "thinking_delta" => events.push(ProviderEvent::ThinkingDelta(text("thinking"))),
                "signature_delta" => {
                    events.push(ProviderEvent::ThinkingSignature(text("signature")))
                }
                "input_json_delta" => {
                    if let Some(id) = state.tool_ids_by_index.get(&index) {
                        events.push(ProviderEvent::ToolUseInputDelta {
                            id: id.clone(),
                            partial_json: text("partial_json"),
                        });
                    }
                }
                _ => {}
            }
        }
        StreamEvent::ContentBlockStop { index } => {
            if let Some(id) = state.tool_ids_by_index.remove(&index) {
                events.push(ProviderEvent::ToolUseEnd { id });
            }
        }
        StreamEvent::MessageDelta { delta, usage } => {
            if let Some(usage) = usage {
                events.push(ProviderEvent::Usage(usage.into()));
            }
            if let Some(reason) = delta.stop_reason {
                events.push(ProviderEvent::Stop(map_stop_reason(&reason)));
            }
        }
        StreamEvent::Error { error } => return Err(map_stream_error(error)),
        StreamEvent::MessageStop | StreamEvent::Ping | StreamEvent::Unknown => {}
    }
    Ok(events)
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        self.catalog.lookup(PROVIDER_ID, model)
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = build_request_body(&req);
        let request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body);
        let request = http::with_extra_headers(request, &self.extra_headers);
        let response = http::send(request, &cancel).await?;

        let events = async_stream::stream! {
            let mut decoder = SseDecoder::new();
            let mut state = StreamState::default();
            let mut chunks = Box::pin(http::text_chunks(response));
            while let Some(text) = chunks.next().await {
                let text = match text {
                    Ok(text) => text,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                for item in decoder.push(&text) {
                    let SseItem::Data(payload) = item else { continue };
                    match translate_event(&mut state, &payload) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    }
                }
            }
        };
        Ok(http::cancellable(events, cancel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResponseAccumulator;
    use arbe_core::{RequestedToolCall, ToolSpec};

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "claude-sonnet-5".to_string(),
            messages,
            temperature: 0.2,
            max_tokens: 1_000,
            tools: vec![],
            thinking_budget_tokens: None,
        }
    }

    fn body_json(req: &ModelRequest) -> Value {
        serde_json::to_value(build_request_body(req)).unwrap()
    }

    fn call(id: &str) -> RequestedToolCall {
        RequestedToolCall {
            id: id.to_string(),
            name: "read_file".to_string(),
            arguments: json!({"path": "a.rs"}),
        }
    }

    #[test]
    fn hoists_system_messages_and_marks_the_cache_breakpoints() {
        let json = body_json(&request(vec![
            Message::new(Role::System, "be terse"),
            Message::new(Role::User, "hi"),
            Message::new(Role::System, "memory: likes rust"),
        ]));
        assert_eq!(json["system"][0]["text"], "be terse");
        assert_eq!(json["system"][1]["text"], "memory: likes rust");
        assert_eq!(json["system"][1]["cache_control"]["type"], "ephemeral");
        assert!(json["system"][0].get("cache_control").is_none());
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        // The final message is always a breakpoint.
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn tool_results_become_one_merged_user_message_with_results_first() {
        let json = body_json(&request(vec![
            Message::new(Role::User, "read both"),
            Message::assistant_tool_calls(vec![call("t1"), call("t2")]),
            Message::tool_result("t1", "one"),
            Message::tool_result_blocks("t2", vec![ContentBlock::text("missing")], true),
            Message::new(Role::User, "and then?"),
        ]));
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["input"]["path"], "a.rs");
        let user = &messages[2];
        assert_eq!(user["role"], "user");
        let content = user["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "t1");
        assert_eq!(content[0]["content"][0]["text"], "one");
        assert_eq!(content[1]["tool_use_id"], "t2");
        assert_eq!(content[1]["is_error"], true);
        assert_eq!(content[2]["text"], "and then?");
    }

    #[test]
    fn a_conversation_that_starts_with_the_assistant_gets_a_placeholder_user_turn() {
        let json = body_json(&request(vec![
            Message::new(Role::Assistant, "as I said"),
            Message::new(Role::User, "go on"),
        ]));
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][1]["role"], "assistant");
    }

    #[test]
    fn only_signed_thinking_and_own_opaque_blocks_are_sent_back() {
        let assistant = Message::with_blocks(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    text: "unsigned".into(),
                    signature: None,
                },
                ContentBlock::Thinking {
                    text: "signed".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    data: json!({"type": "redacted_thinking", "data": "xyz"}),
                },
                ContentBlock::Opaque {
                    provider: "other".into(),
                    data: json!({"type": "whatever"}),
                },
                ContentBlock::text(""),
                ContentBlock::text("answer"),
            ],
        );
        let json = body_json(&request(vec![Message::new(Role::User, "q"), assistant]));
        let content = json["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["type"], "redacted_thinking");
        assert_eq!(content[2]["text"], "answer");
    }

    #[test]
    fn non_object_tool_input_is_wrapped() {
        let bad = RequestedToolCall {
            id: "t".into(),
            name: "x".into(),
            arguments: json!("{oops"),
        };
        let json = body_json(&request(vec![
            Message::new(Role::User, "q"),
            Message::assistant_tool_calls(vec![bad]),
        ]));
        assert_eq!(
            json["messages"][1]["content"][0]["input"]["raw_arguments"],
            "{oops"
        );
    }

    #[test]
    fn tools_use_input_schema_and_auto_choice() {
        let mut req = request(vec![Message::new(Role::User, "q")]);
        req.tools.push(ToolSpec {
            name: "glob".into(),
            description: "find files".into(),
            parameters: json!({"type": "object"}),
        });
        let json = body_json(&req);
        assert_eq!(json["tools"][0]["name"], "glob");
        assert_eq!(json["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(json["tool_choice"]["type"], "auto");
        assert_eq!(json["temperature"].as_f64().unwrap() as f32, 0.2);
    }

    #[test]
    fn thinking_adds_its_budget_to_max_tokens_and_drops_temperature() {
        let mut req = request(vec![Message::new(Role::User, "q")]);
        req.thinking_budget_tokens = Some(4_000);
        let json = body_json(&req);
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 4_000);
        assert_eq!(json["max_tokens"], 5_000);
        assert!(json.get("temperature").is_none());
    }

    fn translate_all(payloads: &[&str]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        payloads
            .iter()
            .flat_map(|p| translate_event(&mut state, p).unwrap())
            .collect()
    }

    /// A recorded-shape stream: thinking, text, and a tool call whose JSON
    /// arrives in fragments, folded through the accumulator.
    #[test]
    fn translates_a_full_stream_with_thinking_text_and_a_tool_call() {
        let events = translate_all(&[
            r#"{"type":"message_start","message":{"id":"m","role":"assistant","content":[],"usage":{"input_tokens":50,"output_tokens":1,"cache_read_input_tokens":1000,"cache_creation_input_tokens":20}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Need the file."}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQB"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading it."}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\": \"sr"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"c/main.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ]);
        let mut acc = ResponseAccumulator::new();
        for e in events {
            acc.push(e);
        }
        let r = acc.finish();

        assert_eq!(
            r.message.content[0],
            ContentBlock::Thinking {
                text: "Need the file.".into(),
                signature: Some("EqQB".into())
            }
        );
        assert_eq!(r.message.text(), "Reading it.");
        let calls = r.message.tool_uses();
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].arguments, json!({"path": "src/main.rs"}));
        assert_eq!(r.stop_reason, StopReason::ToolUse);
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 50,
                output_tokens: 42,
                cache_read_tokens: 1000,
                cache_write_tokens: 20,
            }
        );
    }

    #[test]
    fn redacted_thinking_round_trips_as_an_opaque_block() {
        let events = translate_all(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"abc"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
        ]);
        assert_eq!(
            events,
            vec![ProviderEvent::Opaque {
                provider: "anthropic".into(),
                data: json!({"type": "redacted_thinking", "data": "abc"}),
            }]
        );
    }

    #[test]
    fn a_stream_error_event_maps_into_the_taxonomy() {
        let mut state = StreamState::default();
        let err = translate_event(
            &mut state,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ProviderError::Overloaded(_)));
        assert!(err.is_retryable());
    }

    #[test]
    fn unknown_event_types_are_ignored() {
        assert!(translate_all(&[r#"{"type":"some_future_event","x":1}"#]).is_empty());
    }

    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_stop_reason("end_turn"), StopReason::EndTurn);
        assert_eq!(map_stop_reason("max_tokens"), StopReason::MaxTokens);
        assert_eq!(map_stop_reason("refusal"), StopReason::Refusal);
        assert_eq!(
            map_stop_reason("pause_turn"),
            StopReason::Other("pause_turn".into())
        );
    }
}
