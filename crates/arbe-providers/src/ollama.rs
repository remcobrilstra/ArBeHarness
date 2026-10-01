use std::collections::HashSet;
use std::sync::Mutex;

use arbe_core::{ContentBlock, ImageSource, Message, ProviderError, Role, StopReason, Usage};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::catalog::ModelCatalog;
use crate::text_tool_calls::TextToolCallFilter;
use crate::{
    ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent, ProviderStream, http,
    next_call_id,
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// The context window requested (`options.num_ctx`) for a model the
/// catalog has no entry for. Ollama's own default is smaller and varies by
/// version, so it's always set explicitly — and taken from
/// `capabilities(model)`, so what the harness budgets for and what the
/// server allocates always agree (override per model via the catalog).
pub(crate) const CONTEXT_WINDOW: u64 = 16_384;

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    /// Models that answered a tool-bearing request with "does not support
    /// tools". Not every Ollama model supports tool calling (e.g. the
    /// original `llama3` doesn't, `llama3.1` does), and nothing tells us
    /// up front, so this learns it per model: the first rejection is
    /// retried without tools, and later requests for that model skip them.
    tools_unsupported: Mutex<HashSet<String>>,
    catalog: ModelCatalog,
}

impl OllamaProvider {
    pub fn new() -> Self {
        Self {
            client: http::client(),
            base_url: DEFAULT_BASE_URL.to_string(),
            tools_unsupported: Mutex::new(HashSet::new()),
            catalog: ModelCatalog::new(),
        }
    }

    pub fn with_catalog(mut self, catalog: ModelCatalog) -> Self {
        self.catalog = catalog;
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn model_lacks_tools(&self, model: &str) -> bool {
        self.tools_unsupported
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(model)
    }

    fn mark_model_lacks_tools(&self, model: &str) {
        self.tools_unsupported
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(model.to_string());
    }

    async fn post_chat(
        &self,
        body: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ProviderError> {
        let request = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(body);
        http::send(request, cancel).await
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Request mapping
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ChatMessage {
    role: String,
    /// Absent (or empty) on an assistant message that only calls tools.
    #[serde(default)]
    content: String,
    /// Base64 image data (Ollama's only image form).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
    /// On a `role: "tool"` message: which tool produced this result.
    /// Ollama matches results to calls by name, not by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
    /// Reasoning text, sent by thinking-capable models.
    #[serde(default, skip_serializing)]
    thinking: Option<String>,
}

/// Wire shape of `message.tool_calls[]`. Unlike OpenAI, Ollama sends
/// `arguments` as an inline JSON object and gives calls no id.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OllamaToolFunction,
}

#[derive(Debug, Serialize)]
struct OllamaToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize)]
struct ChatOptions {
    temperature: f32,
    num_predict: u64,
    num_ctx: u64,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaTool>,
    options: ChatOptions,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn tool_result_text(content: &[ContentBlock], is_error: bool) -> String {
    let mut out = String::new();
    if is_error {
        out.push_str("[error] ");
    }
    for block in content {
        match block {
            ContentBlock::Text { text } => out.push_str(text),
            ContentBlock::Image {
                source: ImageSource::Base64 { .. },
                ..
            } => out.push_str("[image: in the next message]"),
            ContentBlock::Image { .. } => out.push_str("[image omitted]"),
            _ => {}
        }
    }
    out
}

/// One harness message becomes zero or more Ollama messages: each tool
/// result is its own `role: "tool"` message named after the tool that was
/// called, and the rest (text, base64 images, tool calls) form one message.
/// Thinking, URL images and opaque blocks are dropped.
fn to_chat_messages(message: &Message, all: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut images = Vec::new();
    let mut tool_calls = Vec::new();
    // Images a tool returned (base64 only: Ollama can't fetch URLs) go in
    // a user message after the tool messages.
    let mut tool_images = Vec::new();

    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::Image {
                source: ImageSource::Base64 { data },
                ..
            } => images.push(data.clone()),
            ContentBlock::ToolUse { name, input, .. } => tool_calls.push(OllamaToolCall {
                function: OllamaFunctionCall {
                    name: name.clone(),
                    arguments: input.clone(),
                },
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                out.push(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_result_text(content, *is_error),
                    tool_name: tool_name_for(all, tool_use_id),
                    ..Default::default()
                });
                tool_images.extend(content.iter().filter_map(|block| match block {
                    ContentBlock::Image {
                        source: ImageSource::Base64 { data },
                        ..
                    } => Some(data.clone()),
                    _ => None,
                }));
            }
            ContentBlock::Image { .. }
            | ContentBlock::Thinking { .. }
            | ContentBlock::Opaque { .. } => {}
        }
    }

    let has_body = !text.is_empty() || !images.is_empty() || !tool_calls.is_empty();
    if has_body && message.role != Role::Tool {
        out.push(ChatMessage {
            role: role_str(message.role).to_string(),
            content: text,
            images,
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            ..Default::default()
        });
    }
    if !tool_images.is_empty() {
        out.push(ChatMessage {
            role: "user".to_string(),
            content: "Images returned by the tool calls above:".to_string(),
            images: tool_images,
            ..Default::default()
        });
    }
    out
}

/// Recovers a tool call's name from the assistant message that requested
/// it, since Ollama identifies results by name rather than call id.
fn tool_name_for(messages: &[Message], tool_use_id: &str) -> Option<String> {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolUse { id, name, .. } if id == tool_use_id => Some(name.clone()),
            _ => None,
        })
}

fn build_request_body(req: &ModelRequest, context_window: u64) -> ChatRequest {
    ChatRequest {
        model: req.model.clone(),
        messages: req
            .messages
            .iter()
            .flat_map(|m| to_chat_messages(m, &req.messages))
            .collect(),
        stream: true,
        tools: req
            .tools
            .iter()
            .map(|t| OllamaTool {
                kind: "function",
                function: OllamaToolFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect(),
        options: ChatOptions {
            temperature: req.temperature,
            num_predict: req.max_tokens,
            num_ctx: context_window,
        },
    }
}

/// Whether an error is Ollama rejecting `tools` for a model that has no
/// tool-calling support (HTTP 400, "... does not support tools").
fn is_tools_unsupported(err: &ProviderError) -> bool {
    matches!(err, ProviderError::InvalidRequest(body) if body.contains("does not support tools"))
}

// ---------------------------------------------------------------------------
// Stream translation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ChatResponseLine {
    #[serde(default)]
    message: Option<ChatMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: u64,
    #[serde(default)]
    eval_count: u64,
    /// Ollama reports some failures as an `error` line mid-stream.
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug)]
struct StreamState {
    saw_tool_call: bool,
    /// Recognizes tool calls some models write as text (see
    /// `text_tool_calls`).
    text_calls: TextToolCallFilter,
}

impl StreamState {
    fn new(offered_tools: Vec<String>) -> Self {
        Self {
            saw_tool_call: false,
            text_calls: TextToolCallFilter::new(offered_tools),
        }
    }

    fn push_tool_call(&mut self, events: &mut Vec<ProviderEvent>, name: String, arguments: Value) {
        self.saw_tool_call = true;
        let id = next_call_id("ollama");
        events.push(ProviderEvent::ToolUseStart {
            id: id.clone(),
            name,
        });
        events.push(ProviderEvent::ToolUseInputDelta {
            id: id.clone(),
            partial_json: arguments_json(arguments),
        });
        events.push(ProviderEvent::ToolUseEnd { id });
    }
}

/// Some models emit `arguments` as a JSON-encoded string instead of an
/// object; the accumulator parses the JSON text either way, so this only
/// needs to produce that text.
fn arguments_json(arguments: Value) -> String {
    match arguments {
        Value::String(raw) => raw,
        Value::Null => "{}".to_string(),
        other => other.to_string(),
    }
}

/// Translates one NDJSON line into provider events. Ollama sends each
/// tool call whole (never fragmented), so each becomes a complete
/// start/input/end triple.
fn translate_line(
    state: &mut StreamState,
    line: &str,
) -> Result<Vec<ProviderEvent>, ProviderError> {
    let parsed: ChatResponseLine = serde_json::from_str(line)
        .map_err(|e| ProviderError::Internal(format!("failed to parse Ollama stream line: {e}")))?;
    if let Some(error) = parsed.error {
        return Err(ProviderError::Internal(format!("Ollama error: {error}")));
    }
    let mut events = Vec::new();

    if let Some(message) = parsed.message {
        if let Some(thinking) = message.thinking.filter(|t| !t.is_empty()) {
            events.push(ProviderEvent::ThinkingDelta(thinking));
        }
        if !message.content.is_empty() {
            let shown = state.text_calls.push(&message.content);
            if !shown.is_empty() {
                events.push(ProviderEvent::TextDelta(shown));
            }
        }
        for call in message.tool_calls.unwrap_or_default() {
            state.push_tool_call(&mut events, call.function.name, call.function.arguments);
        }
    }

    if parsed.done {
        match state.text_calls.finish() {
            Ok(calls) => {
                tracing::debug!(count = calls.len(), "recognized tool calls written as text");
                for call in calls {
                    state.push_tool_call(&mut events, call.name, call.arguments);
                }
            }
            Err(text) if !text.is_empty() => events.push(ProviderEvent::TextDelta(text)),
            Err(_) => {}
        }
        events.push(ProviderEvent::Usage(Usage {
            input_tokens: parsed.prompt_eval_count,
            output_tokens: parsed.eval_count,
            ..Default::default()
        }));
        // Ollama reports "stop" even when the model called tools.
        let reason = match parsed.done_reason.as_deref() {
            Some("length") => StopReason::MaxTokens,
            _ if state.saw_tool_call => StopReason::ToolUse,
            None | Some("stop") => StopReason::EndTurn,
            Some(other) => StopReason::Other(other.to_string()),
        };
        events.push(ProviderEvent::Stop(reason));
    }
    Ok(events)
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> &str {
        "ollama"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        let mut caps = self.catalog.lookup("ollama", model);
        caps.tool_calls &= !self.model_lacks_tools(model);
        caps
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let context_window = self.capabilities(&req.model).max_context_tokens;
        let mut body = build_request_body(&req, context_window);
        if self.model_lacks_tools(&req.model) {
            body.tools.clear();
        }
        let response = match self.post_chat(&body, &cancel).await {
            Err(err) if !body.tools.is_empty() && is_tools_unsupported(&err) => {
                tracing::info!(
                    model = %req.model,
                    "model does not support tool calling; retrying without tools"
                );
                self.mark_model_lacks_tools(&req.model);
                body.tools.clear();
                self.post_chat(&body, &cancel).await?
            }
            other => other?,
        };

        let offered_tools: Vec<String> =
            body.tools.iter().map(|t| t.function.name.clone()).collect();
        let events = async_stream::stream! {
            let mut buffer = String::new();
            let mut state = StreamState::new(offered_tools);
            let mut chunks = Box::pin(http::text_chunks(response));
            while let Some(text) = chunks.next().await {
                match text {
                    Ok(text) => buffer.push_str(&text),
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer.drain(..=newline_pos);
                    if line.is_empty() {
                        continue;
                    }
                    match translate_line(&mut state, &line) {
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
    use serde_json::json;

    fn request(messages: Vec<Message>, tools: Vec<ToolSpec>) -> ModelRequest {
        ModelRequest {
            model: "llama3.1".to_string(),
            messages,
            temperature: 0.2,
            max_tokens: 100,
            tools,
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
        let json = body_json(&request(vec![image_result()], vec![]));
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["images"], json!(["QUJD"]));
    }

    fn body_json(req: &ModelRequest) -> serde_json::Value {
        serde_json::to_value(build_request_body(req, CONTEXT_WINDOW)).unwrap()
    }

    #[test]
    fn builds_request_body_with_mapped_roles() {
        let json = body_json(&request(vec![Message::new(Role::User, "hi")], vec![]));
        assert_eq!(json["model"], "llama3.1");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn sends_temperature_output_cap_and_context_window_as_options() {
        let json = body_json(&request(vec![], vec![]));
        assert_eq!(json["options"]["num_predict"], 100);
        assert_eq!(json["options"]["num_ctx"], CONTEXT_WINDOW);
        assert!((json["options"]["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        // No tools offered -> no `tools` key at all.
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn offers_tools_in_ollama_function_format() {
        let tools = vec![ToolSpec {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let json = body_json(&request(vec![], tools));
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
        assert_eq!(
            json["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
    }

    #[test]
    fn echoes_tool_calls_and_names_tool_results_by_their_call() {
        let call = RequestedToolCall {
            id: "ollama-call-7".to_string(),
            name: "read_file".to_string(),
            arguments: json!({"path": "a.txt"}),
        };
        let messages = vec![
            Message::new(Role::User, "read a.txt"),
            Message::assistant_tool_calls(vec![call]),
            Message::tool_result("ollama-call-7", "contents"),
        ];
        let json = body_json(&request(messages, vec![]));
        let assistant = &json["messages"][1];
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read_file");
        // Arguments go back as an inline object, not a JSON string.
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"]["path"],
            "a.txt"
        );
        let tool = &json["messages"][2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_name"], "read_file");
        assert_eq!(tool["content"], "contents");
    }

    #[test]
    fn base64_images_go_in_the_images_field() {
        let msg = Message::with_blocks(
            Role::User,
            vec![
                ContentBlock::text("describe"),
                ContentBlock::Image {
                    source: ImageSource::Base64 {
                        data: "AAAA".into(),
                    },
                    media_type: "image/png".into(),
                },
            ],
        );
        let json = body_json(&request(vec![msg], vec![]));
        assert_eq!(json["messages"][0]["images"][0], "AAAA");
        assert_eq!(json["messages"][0]["content"], "describe");
    }

    fn translate_all(lines: &[&str]) -> Vec<ProviderEvent> {
        translate_offering(&[], lines)
    }

    fn translate_offering(tools: &[&str], lines: &[&str]) -> Vec<ProviderEvent> {
        let mut state = StreamState::new(tools.iter().map(|t| t.to_string()).collect());
        lines
            .iter()
            .flat_map(|l| translate_line(&mut state, l).unwrap())
            .collect()
    }

    #[test]
    fn translates_text_then_done_with_usage() {
        let events = translate_all(&[
            r#"{"message":{"role":"assistant","content":"Hel"},"done":false}"#,
            r#"{"message":{"role":"assistant","content":"lo"},"done":false}"#,
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":12,"eval_count":3}"#,
        ]);
        assert_eq!(
            events,
            vec![
                ProviderEvent::TextDelta("Hel".into()),
                ProviderEvent::TextDelta("lo".into()),
                ProviderEvent::Usage(Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    ..Default::default()
                }),
                ProviderEvent::Stop(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn tool_calls_become_complete_tool_uses_and_stop_reason_tool_use() {
        let events = translate_all(&[
            r#"{"message":{"role":"assistant","content":"","tool_calls":[
                {"function":{"name":"read_file","arguments":{"path":"a.txt"}}},
                {"function":{"name":"list_dir","arguments":{}}}
            ]},"done":false}"#,
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"}"#,
        ]);
        let mut acc = ResponseAccumulator::new();
        for e in events {
            acc.push(e);
        }
        let r = acc.finish();
        let calls = r.message.tool_uses();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, json!({"path": "a.txt"}));
        assert_ne!(calls[0].id, calls[1].id);
        assert_eq!(r.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn a_tool_call_written_as_text_becomes_a_tool_use() {
        // What qwen2.5-coder:7b actually streamed for a get_weather request.
        let lines = [
            r#"{"message":{"role":"assistant","content":"{\"name\": \"get_weather\", "},"done":false}"#,
            r#"{"message":{"role":"assistant","content":"\"arguments\": {\"city\": \"Paris\"}}"},"done":false}"#,
            r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop"}"#,
        ];
        let mut acc = ResponseAccumulator::new();
        for e in translate_offering(&["get_weather"], &lines) {
            acc.push(e);
        }
        let r = acc.finish();
        assert_eq!(r.message.text(), "");
        let calls = r.message.tool_uses();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments, json!({"city": "Paris"}));
        assert_eq!(r.stop_reason, StopReason::ToolUse);

        // Without the tool offered, the same text is just text.
        let mut acc = ResponseAccumulator::new();
        for e in translate_all(&lines) {
            acc.push(e);
        }
        let r = acc.finish();
        assert!(r.message.text().starts_with("{\"name\""));
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn string_encoded_and_missing_arguments_are_handled() {
        assert_eq!(
            arguments_json(json!("{\"path\":\"a\"}")),
            "{\"path\":\"a\"}"
        );
        assert_eq!(arguments_json(Value::Null), "{}");
        assert_eq!(arguments_json(json!({"a": 1})), "{\"a\":1}");
    }

    #[test]
    fn thinking_field_becomes_thinking_deltas() {
        let events = translate_all(&[
            r#"{"message":{"role":"assistant","content":"","thinking":"hmm"},"done":false}"#,
        ]);
        assert_eq!(events, vec![ProviderEvent::ThinkingDelta("hmm".into())]);
    }

    #[test]
    fn length_done_reason_is_max_tokens() {
        let events = translate_all(&[r#"{"done":true,"done_reason":"length"}"#]);
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Stop(StopReason::MaxTokens))
        );
    }

    #[test]
    fn an_error_line_is_an_error() {
        let mut state = StreamState::new(Vec::new());
        assert!(translate_line(&mut state, r#"{"error":"model crashed"}"#).is_err());
    }

    #[test]
    fn recognizes_the_tools_unsupported_rejection() {
        let err = crate::error_map::map_http_error(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"registry.ollama.ai/library/llama3:latest does not support tools"}"#,
        );
        assert!(is_tools_unsupported(&err));
        assert!(!is_tools_unsupported(&ProviderError::InvalidRequest(
            "bad model name".to_string()
        )));
    }

    #[test]
    fn remembers_models_without_tool_support_and_reports_it_in_capabilities() {
        let provider = OllamaProvider::new();
        assert!(provider.capabilities("llama3").tool_calls);
        provider.mark_model_lacks_tools("llama3");
        assert!(!provider.capabilities("llama3").tool_calls);
        assert!(provider.capabilities("llama3.1").tool_calls);
    }
}
