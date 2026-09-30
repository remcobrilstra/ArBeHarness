use std::collections::HashMap;

use arbe_core::{ContentBlock, ImageSource, Message, ProviderError, Role, StopReason, Usage};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::catalog::ModelCatalog;
use crate::sse::{SseDecoder, SseItem};
use crate::{
    ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent, ProviderStream, http,
    next_call_id,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI Chat Completions adapter. Also the adapter for any
/// OpenAI-compatible endpoint (via `with_base_url`).
pub struct OpenAiProvider {
    /// `"openai"`, or `"openai_compatible"` for other servers speaking the
    /// same API (which changes catalog lookups and makes the key optional).
    id: &'static str,
    client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    extra_headers: Vec<(String, String)>,
    catalog: ModelCatalog,
}

impl OpenAiProvider {
    /// `api_key` should come from env/config indirection per NFR-4 — never
    /// hardcode or log it.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            id: "openai",
            client: http::client(),
            api_key: Some(api_key.into()),
            base_url: DEFAULT_BASE_URL.to_string(),
            extra_headers: Vec::new(),
            catalog: ModelCatalog::new(),
        }
    }

    /// Any server implementing the Chat Completions API (vLLM, LM Studio,
    /// llama.cpp, OpenRouter, ...). Local servers often need no key.
    pub fn compatible(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            id: "openai_compatible",
            api_key: api_key.filter(|k| !k.is_empty()),
            base_url: base_url.into(),
            ..Self::new("")
        }
    }

    /// Overrides the API base URL, e.g. to point at a compatible gateway.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Headers sent on every request (gateway routing, attribution, ...).
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

#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<ChatContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// Plain string content, or a list of parts when the message has images.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum ChatContent {
    Text(String),
    Parts(Vec<ChatPart>),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ChatPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolCallFunction,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolCallFunction {
    name: String,
    /// OpenAI encodes call arguments as a JSON string, not an inline object.
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ChatToolFunction,
}

#[derive(Debug, Serialize)]
struct ChatToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    /// Newer models (o1/o3/gpt-5 family) reject the legacy `max_tokens`
    /// field with a 400 ("Unsupported parameter") and require
    /// `max_completion_tokens` instead; OpenAI's Chat Completions API
    /// accepts `max_completion_tokens` across current models, so it's used
    /// unconditionally rather than branching on model name.
    max_completion_tokens: u64,
    stream: bool,
    /// Asks for a final usage chunk on the stream.
    stream_options: StreamOptions,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn image_url(source: &ImageSource, media_type: &str) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { data } => format!("data:{media_type};base64,{data}"),
    }
}

/// The text a tool-result message carries. OpenAI tool messages are
/// text-only and have no error flag, so errors are marked in the text and
/// non-text blocks are noted rather than silently dropped.
fn tool_result_text(content: &[ContentBlock], is_error: bool) -> String {
    let mut out = String::new();
    if is_error {
        out.push_str("[error] ");
    }
    for block in content {
        match block {
            ContentBlock::Text { text } => out.push_str(text),
            ContentBlock::Image { .. } => out.push_str("[image: in the next message]"),
            _ => {}
        }
    }
    out
}

/// One harness message becomes zero or more chat messages: each tool
/// result is its own `role: "tool"` message (OpenAI's convention), and the
/// rest — text, images, tool calls — form one message for the role.
/// Thinking and opaque blocks have no Chat Completions equivalent and are
/// dropped.
fn to_chat_messages(message: &Message) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut parts = Vec::new();
    let mut has_image = false;
    let mut tool_calls = Vec::new();
    // Tool messages can only carry text; images a tool returned follow the
    // tool messages in a user message.
    let mut tool_images = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => {
                text.push_str(t);
                parts.push(ChatPart::Text { text: t.clone() });
            }
            ContentBlock::Image { source, media_type } => {
                has_image = true;
                parts.push(ChatPart::ImageUrl {
                    image_url: ImageUrl {
                        url: image_url(source, media_type),
                    },
                });
            }
            ContentBlock::ToolUse { id, name, input } => tool_calls.push(ChatToolCall {
                id: id.clone(),
                kind: "function",
                function: ChatToolCallFunction {
                    name: name.clone(),
                    arguments: input.to_string(),
                },
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                out.push(ChatMessage {
                    role: "tool",
                    content: Some(ChatContent::Text(tool_result_text(content, *is_error))),
                    tool_calls: None,
                    tool_call_id: Some(tool_use_id.clone()),
                });
                tool_images.extend(content.iter().filter_map(|block| match block {
                    ContentBlock::Image { source, media_type } => Some(ChatPart::ImageUrl {
                        image_url: ImageUrl {
                            url: image_url(source, media_type),
                        },
                    }),
                    _ => None,
                }));
            }
            ContentBlock::Thinking { .. } | ContentBlock::Opaque { .. } => {}
        }
    }

    let has_body = !parts.is_empty() || !tool_calls.is_empty();
    if has_body && message.role != Role::Tool {
        let content = if has_image {
            Some(ChatContent::Parts(parts))
        } else if text.is_empty() && !tool_calls.is_empty() {
            None
        } else {
            Some(ChatContent::Text(text))
        };
        out.push(ChatMessage {
            role: role_str(message.role),
            content,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            tool_call_id: None,
        });
    }
    if !tool_images.is_empty() {
        let mut parts = vec![ChatPart::Text {
            text: "Images returned by the tool calls above:".into(),
        }];
        parts.extend(tool_images);
        out.push(ChatMessage {
            role: "user",
            content: Some(ChatContent::Parts(parts)),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    out
}

fn build_request_body(req: &ModelRequest) -> ChatRequest {
    let tools: Vec<ChatTool> = req
        .tools
        .iter()
        .map(|t| ChatTool {
            kind: "function",
            function: ChatToolFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect();
    let tool_choice = if tools.is_empty() { None } else { Some("auto") };
    ChatRequest {
        model: req.model.clone(),
        messages: req.messages.iter().flat_map(to_chat_messages).collect(),
        temperature: req.temperature,
        max_completion_tokens: req.max_tokens,
        stream: true,
        stream_options: StreamOptions {
            include_usage: true,
        },
        tools,
        tool_choice,
    }
}

// ---------------------------------------------------------------------------
// Stream translation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<ChatUsage>,
    /// Set instead of `choices` when the server fails mid-stream (OpenAI
    /// and compatible servers send `{"error": {...}}` on overload, rate
    /// limits, ...).
    #[serde(default)]
    error: Option<StreamError>,
}

#[derive(Debug, Deserialize)]
struct StreamError {
    #[serde(default)]
    message: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    code: Option<serde_json::Value>,
}

/// A mid-stream error payload in the `ProviderError` taxonomy, so it's
/// reported (and, before any output, retried) for what it is.
fn map_stream_error(error: StreamError) -> ProviderError {
    let code = match &error.code {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    let kind = error.kind.unwrap_or_default();
    let message = if error.message.is_empty() {
        format!("{kind} {code}").trim().to_string()
    } else {
        error.message
    };
    let label = format!("{kind} {code}").to_ascii_lowercase();
    if label.contains("rate_limit") || code == "429" {
        ProviderError::rate_limit(message)
    } else if label.contains("context_length") {
        ProviderError::ContextLengthExceeded(message)
    } else if label.contains("overloaded")
        || label.contains("server_error")
        || ["500", "502", "503", "529"].contains(&code.as_str())
    {
        ProviderError::Overloaded(message)
    } else if label.contains("invalid_request") {
        ProviderError::InvalidRequest(message)
    } else {
        ProviderError::Internal(format!(
            "the provider reported an error mid-stream: {message}"
        ))
    }
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning text, as emitted by several OpenAI-compatible servers
    /// (DeepSeek, vLLM reasoning parsers). OpenAI itself doesn't send it.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamToolCall>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// OpenAI streams tool calls keyed by `index`, with the id and name only on
/// the first delta for each index; this remembers which id each index got.
#[derive(Debug, Default)]
struct StreamState {
    ids_by_index: HashMap<u32, String>,
    open_in_order: Vec<String>,
    /// A `finish_reason` arrived (and with it a `Stop` event).
    finished: bool,
}

/// `[DONE]` is itself a completion marker. Servers that send it without a
/// `finish_reason` still get a `Stop`, inferred from whether the model
/// called tools; otherwise the stream would look cut short.
fn stop_on_done(state: &mut StreamState) -> Vec<ProviderEvent> {
    if state.finished {
        return Vec::new();
    }
    state.finished = true;
    let mut events: Vec<ProviderEvent> = state
        .open_in_order
        .drain(..)
        .map(|id| ProviderEvent::ToolUseEnd { id })
        .collect();
    events.push(ProviderEvent::Stop(if state.ids_by_index.is_empty() {
        StopReason::EndTurn
    } else {
        StopReason::ToolUse
    }));
    events
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        other => StopReason::Other(other.to_string()),
    }
}

fn map_usage(usage: ChatUsage) -> Usage {
    // OpenAI's prompt_tokens includes cached tokens; `Usage::input_tokens`
    // excludes them, so split them out.
    let cached = usage
        .prompt_tokens_details
        .map(|d| d.cached_tokens)
        .unwrap_or(0);
    Usage {
        input_tokens: usage.prompt_tokens.saturating_sub(cached),
        output_tokens: usage.completion_tokens,
        cache_read_tokens: cached,
        cache_write_tokens: 0,
    }
}

/// Translates one SSE `data:` payload into provider events.
fn translate_chunk(
    state: &mut StreamState,
    payload: &str,
) -> Result<Vec<ProviderEvent>, ProviderError> {
    let chunk: StreamChunk = serde_json::from_str(payload).map_err(|e| {
        ProviderError::Internal(format!("failed to parse OpenAI stream chunk: {e}"))
    })?;
    if let Some(error) = chunk.error {
        return Err(map_stream_error(error));
    }
    let mut events = Vec::new();

    for choice in chunk.choices {
        let delta = choice.delta;
        if let Some(reasoning) = delta.reasoning_content.filter(|r| !r.is_empty()) {
            events.push(ProviderEvent::ThinkingDelta(reasoning));
        }
        if let Some(content) = delta.content.filter(|c| !c.is_empty()) {
            events.push(ProviderEvent::TextDelta(content));
        }
        for call in delta.tool_calls {
            let (name, arguments) = match call.function {
                Some(f) => (f.name, f.arguments),
                None => (None, None),
            };
            let id = match state.ids_by_index.get(&call.index) {
                Some(id) => id.clone(),
                None => {
                    // Some compatible servers omit the id; synthesize one.
                    let id = call
                        .id
                        .filter(|id| !id.is_empty())
                        .unwrap_or_else(|| next_call_id("openai"));
                    state.ids_by_index.insert(call.index, id.clone());
                    state.open_in_order.push(id.clone());
                    events.push(ProviderEvent::ToolUseStart {
                        id: id.clone(),
                        name: name.unwrap_or_default(),
                    });
                    id
                }
            };
            if let Some(partial_json) = arguments.filter(|a| !a.is_empty()) {
                events.push(ProviderEvent::ToolUseInputDelta { id, partial_json });
            }
        }
        if let Some(reason) = choice.finish_reason {
            state.finished = true;
            for id in state.open_in_order.drain(..) {
                events.push(ProviderEvent::ToolUseEnd { id });
            }
            events.push(ProviderEvent::Stop(map_finish_reason(&reason)));
        }
    }

    if let Some(usage) = chunk.usage {
        events.push(ProviderEvent::Usage(map_usage(usage)));
    }
    Ok(events)
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        self.catalog.lookup(self.id, model)
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let body = build_request_body(&req);
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let request = http::with_extra_headers(request, &self.extra_headers);
        let response = http::send(request, &cancel).await?;
        Ok(stream_sse_chat(response, cancel))
    }
}

/// The Chat Completions body for `req`. Shared with the Grok subscription
/// adapter, which speaks the same stream.
pub(crate) fn chat_completion_body(req: &ModelRequest) -> serde_json::Value {
    serde_json::to_value(build_request_body(req)).expect("chat request serializes")
}

/// SSE `data:` chunks from a Chat Completions response, as provider events.
pub(crate) fn stream_sse_chat(
    response: reqwest::Response,
    cancel: CancellationToken,
) -> ProviderStream {
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
                let payload = match item {
                    SseItem::Done => {
                        for event in stop_on_done(&mut state) {
                            yield Ok(event);
                        }
                        return;
                    }
                    SseItem::Data(payload) => payload,
                };
                match translate_chunk(&mut state, &payload) {
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
    http::cancellable(events, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResponseAccumulator;
    use arbe_core::{RequestedToolCall, ToolSpec};
    use serde_json::json;

    fn base_req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "gpt-5".to_string(),
            messages,
            temperature: 0.2,
            max_tokens: 100,
            tools: Vec::new(),
            thinking_budget_tokens: None,
        }
    }

    fn image_result() -> Message {
        Message::tool_result_blocks(
            "call_1",
            vec![
                ContentBlock::text("{\"image\":\"shot.png\"}"),
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        data: "QUJD".into(),
                    },
                    media_type: "image/png".into(),
                },
            ],
            false,
        )
    }

    #[test]
    fn images_from_tool_results_follow_in_a_user_message() {
        let json = body_json(&base_req(vec![image_result()]));
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "tool");
        assert!(
            messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("[image: in the next message]")
        );
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(
            messages[1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
    }

    fn body_json(req: &ModelRequest) -> serde_json::Value {
        serde_json::to_value(build_request_body(req)).unwrap()
    }

    #[test]
    fn builds_request_body_with_mapped_roles_and_usage_streaming() {
        let req = base_req(vec![
            Message::new(Role::System, "be terse"),
            Message::new(Role::User, "hi"),
        ]);
        let json = body_json(&req);
        assert_eq!(json["model"], "gpt-5");
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][0]["content"], "be terse");
        assert_eq!(json["messages"][1]["role"], "user");
        assert_eq!(json["stream"], true);
        assert_eq!(json["stream_options"]["include_usage"], true);
        assert!(json.get("tools").is_none());
        assert!(json.get("tool_choice").is_none());
    }

    #[test]
    fn attaches_tools_and_tool_choice_when_tools_are_offered() {
        let mut req = base_req(vec![Message::new(Role::User, "read main.rs")]);
        req.tools.push(ToolSpec {
            name: "read_file".to_string(),
            description: "reads a file".to_string(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        });
        let json = body_json(&req);
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
        assert_eq!(json["tool_choice"], "auto");
    }

    #[test]
    fn assistant_tool_call_message_omits_content_and_carries_tool_calls() {
        let req = base_req(vec![Message::assistant_tool_calls(vec![
            RequestedToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: json!({"path": "main.rs"}),
            },
        ])]);
        let json = body_json(&req);
        let msg = &json["messages"][0];
        assert!(msg.get("content").is_none());
        assert_eq!(msg["tool_calls"][0]["id"], "call_1");
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"main.rs"}"#
        );
    }

    #[test]
    fn each_tool_result_becomes_its_own_tool_message() {
        let msg = Message::with_blocks(
            Role::Tool,
            vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: vec![ContentBlock::text("file contents")],
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_2".into(),
                    content: vec![ContentBlock::text("no such file")],
                    is_error: true,
                },
            ],
        );
        let json = body_json(&base_req(vec![msg]));
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_1");
        assert_eq!(messages[0]["content"], "file contents");
        assert_eq!(messages[1]["tool_call_id"], "call_2");
        assert_eq!(messages[1]["content"], "[error] no such file");
    }

    #[test]
    fn images_switch_content_to_parts_with_data_urls() {
        let msg = Message::with_blocks(
            Role::User,
            vec![
                ContentBlock::text("what is this?"),
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        data: "AAAA".into(),
                    },
                    media_type: "image/png".into(),
                },
            ],
        );
        let json = body_json(&base_req(vec![msg]));
        let parts = &json["messages"][0]["content"];
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn thinking_blocks_are_not_sent() {
        let msg = Message::with_blocks(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    text: "hmm".into(),
                    signature: None,
                },
                ContentBlock::text("answer"),
            ],
        );
        let json = body_json(&base_req(vec![msg]));
        assert_eq!(json["messages"][0]["content"], "answer");
    }

    fn translate_all(payloads: &[&str]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        payloads
            .iter()
            .flat_map(|p| translate_chunk(&mut state, p).unwrap())
            .collect()
    }

    #[test]
    fn translates_text_deltas_finish_and_usage() {
        let events = translate_all(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":4}}}"#,
        ]);
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo".into()),
                ProviderEvent::Stop(StopReason::EndTurn),
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 2,
                    cache_read_tokens: 4,
                    cache_write_tokens: 0,
                }),
            ]
        );
    }

    #[test]
    fn reassembles_streamed_parallel_tool_calls_via_the_accumulator() {
        let events = translate_all(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"list_dir","arguments":"{}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ]);
        let mut acc = ResponseAccumulator::new();
        for e in events {
            acc.push(e);
        }
        let r = acc.finish();
        let calls = r.message.tool_uses();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[0].arguments, json!({"path": "a.rs"}));
        assert_eq!(calls[1].id, "call_b");
        assert_eq!(calls[1].name, "list_dir");
        assert_eq!(r.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn a_tool_call_without_an_id_gets_a_synthesized_one() {
        let events = translate_all(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"glob","arguments":"{}"}}]}}]}"#,
        ]);
        let ProviderEvent::ToolUseStart { id, name } = &events[0] else {
            panic!("expected a tool use start, got {events:?}");
        };
        assert!(id.starts_with("openai-call-"));
        assert_eq!(name, "glob");
    }

    #[test]
    fn reasoning_content_from_compatible_servers_becomes_thinking() {
        let events = translate_all(&[r#"{"choices":[{"delta":{"reasoning_content":"think"}}]}"#]);
        assert_eq!(events, vec![ProviderEvent::ThinkingDelta("think".into())]);
    }

    #[test]
    fn maps_finish_reasons() {
        assert_eq!(map_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_finish_reason("content_filter"), StopReason::Refusal);
        assert_eq!(
            map_finish_reason("weird"),
            StopReason::Other("weird".into())
        );
    }

    #[test]
    fn malformed_chunk_is_an_internal_error() {
        let mut state = StreamState::default();
        assert!(matches!(
            translate_chunk(&mut state, "{not json"),
            Err(ProviderError::Internal(_))
        ));
    }

    #[test]
    fn an_error_sent_mid_stream_is_reported_for_what_it_is() {
        let mut state = StreamState::default();
        let error =
            |payload: &str| translate_chunk(&mut StreamState::default(), payload).unwrap_err();
        assert!(matches!(
            translate_chunk(
                &mut state,
                r#"{"error":{"message":"The server is overloaded","type":"server_error"}}"#
            ),
            Err(ProviderError::Overloaded(ref m)) if m == "The server is overloaded"
        ));
        assert!(
            error(r#"{"error":{"message":"slow down","code":"rate_limit_exceeded"}}"#)
                .is_retryable()
        );
        assert!(matches!(
            error(r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#),
            ProviderError::ContextLengthExceeded(_)
        ));
        assert!(matches!(
            error(r#"{"error":{"message":"odd","code":500}}"#),
            ProviderError::Overloaded(_)
        ));
        assert!(matches!(
            error(r#"{"error":{"message":"what"}}"#),
            ProviderError::Internal(ref m) if m.contains("what")
        ));
    }
}
