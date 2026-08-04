use arbe_core::{ProviderError, RequestedToolCall, Role};
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::error_map::{map_http_error, map_transport_error};
use crate::sse::{SseDecoder, SseItem};
use crate::utf8_buffer::Utf8ChunkBuffer;
use crate::{ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, TokenChunk};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiProvider {
    /// `api_key` should come from env/config indirection per NFR-4 — never
    /// hardcode or log it.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Overrides the API base URL, e.g. to point at a compatible gateway.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ChatMessage {
    role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

/// Wire shape of `message.tool_calls[]` / the request-side echo of a
/// previously-requested call — identical on both sides of the round trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatToolCall {
    id: String,
    #[serde(rename = "type", default = "function_type")]
    kind: String,
    function: ChatToolCallFunction,
}

fn function_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatToolCallFunction {
    name: String,
    /// OpenAI encodes call arguments as a JSON string, not an inline
    /// object, on both the request and response sides.
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

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

fn build_request_body(req: &ModelRequest, stream: bool) -> ChatRequest {
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
        messages: req
            .messages
            .iter()
            .map(|m| ChatMessage {
                role: role_str(m.role).to_string(),
                content: if m.content.is_empty() && m.tool_calls.is_some() {
                    None
                } else {
                    Some(m.content.clone())
                },
                tool_calls: m.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|c| ChatToolCall {
                            id: c.id.clone(),
                            kind: function_type(),
                            function: ChatToolCallFunction {
                                name: c.name.clone(),
                                arguments: c.arguments.to_string(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: m.tool_call_id.clone(),
            })
            .collect(),
        temperature: req.temperature,
        max_completion_tokens: req.max_tokens,
        stream,
        tools,
        tool_choice,
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

/// Parses a tool call's JSON-string `arguments` into a `Value`. A model
/// occasionally emits malformed JSON for a call's arguments; rather than
/// fail the whole response over one bad call, this falls back to wrapping
/// the raw string so the tool executor's own argument validation reports
/// the problem (with the original text visible) instead of a provider
/// parse error swallowing it.
fn parse_tool_call_arguments(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

fn parse_response(body: &str) -> Result<ModelResponse, ProviderError> {
    let parsed: ChatResponse = serde_json::from_str(body)
        .map_err(|e| ProviderError::Internal(format!("failed to parse OpenAI response: {e}")))?;
    let message = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message)
        .ok_or_else(|| ProviderError::Internal("OpenAI response had no choices".to_string()))?;
    let tool_calls = message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tc| RequestedToolCall {
            id: tc.id,
            name: tc.function.name,
            arguments: parse_tool_call_arguments(&tc.function.arguments),
        })
        .collect();
    Ok(ModelResponse {
        content: message.content.unwrap_or_default(),
        tool_calls,
    })
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    content: Option<String>,
}

fn parse_stream_payload(payload: &str) -> Result<Option<TokenChunk>, ProviderError> {
    let chunk: StreamChunk = serde_json::from_str(payload).map_err(|e| {
        ProviderError::Internal(format!("failed to parse OpenAI stream chunk: {e}"))
    })?;
    let delta = chunk
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.delta.content);
    Ok(delta.map(|delta| TokenChunk {
        delta,
        is_final: false,
    }))
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calls: true,
            json_mode: true,
            max_context_tokens: 128_000,
        }
    }

    async fn infer(&self, req: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = build_request_body(&req, false);
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        let text = response.text().await.map_err(map_transport_error)?;
        if !status.is_success() {
            return Err(map_http_error(status, &text));
        }
        parse_response(&text)
    }

    async fn infer_stream(
        &self,
        req: ModelRequest,
    ) -> Result<
        Box<dyn Stream<Item = Result<TokenChunk, ProviderError>> + Send + Unpin>,
        ProviderError,
    > {
        let body = build_request_body(&req, true);
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(map_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.map_err(map_transport_error)?;
            return Err(map_http_error(status, &text));
        }

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            let mut decoder = SseDecoder::new();
            let mut utf8_buf = Utf8ChunkBuffer::new();
            let mut bytes_stream = response.bytes_stream();
            'outer: while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(map_transport_error(e));
                        break;
                    }
                };
                let text = utf8_buf.push(&chunk);
                for item in decoder.push(&text) {
                    match item {
                        SseItem::Done => {
                            // Unlike Ollama (which reports `done` inline on
                            // the last content chunk), OpenAI's `[DONE]` is
                            // a separate, content-less sentinel — emit an
                            // explicit is_final chunk here so `TokenChunk`'s
                            // is_final contract is meaningful for both
                            // providers rather than always false for OpenAI.
                            yield Ok(TokenChunk {
                                delta: String::new(),
                                is_final: true,
                            });
                            break 'outer;
                        }
                        SseItem::Data(payload) => match parse_stream_payload(&payload) {
                            Ok(Some(token)) => yield Ok(token),
                            Ok(None) => {}
                            Err(e) => {
                                yield Err(e);
                                break 'outer;
                            }
                        },
                    }
                }
            }
        };

        Ok(Box::new(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{Message, ToolSpec};
    use serde_json::json;

    fn base_req(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "gpt-5".to_string(),
            messages,
            temperature: 0.2,
            max_tokens: 100,
            tools: Vec::new(),
        }
    }

    #[test]
    fn builds_request_body_with_mapped_roles() {
        let req = base_req(vec![
            Message::new(Role::System, "be terse"),
            Message::new(Role::User, "hi"),
        ]);
        let body = build_request_body(&req, false);
        assert_eq!(body.model, "gpt-5");
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
        assert!(!body.stream);
        assert!(body.tools.is_empty());
        assert!(body.tool_choice.is_none());
    }

    #[test]
    fn attaches_tools_and_tool_choice_when_tools_are_offered() {
        let mut req = base_req(vec![Message::new(Role::User, "read main.rs")]);
        req.tools.push(ToolSpec {
            name: "read_file".to_string(),
            description: "reads a file".to_string(),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        });
        let body = build_request_body(&req, false);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].function.name, "read_file");
        assert_eq!(body.tool_choice, Some("auto"));
    }

    #[test]
    fn assistant_tool_call_message_omits_content_and_carries_tool_calls() {
        let req = base_req(vec![Message::assistant_tool_calls(vec![
            arbe_core::RequestedToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: json!({"path": "main.rs"}),
            },
        ])]);
        let body = build_request_body(&req, false);
        assert!(body.messages[0].content.is_none());
        let calls = body.messages[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"main.rs"}"#);
    }

    #[test]
    fn tool_result_message_carries_its_tool_call_id() {
        let req = base_req(vec![Message::tool_result("call_1", "file contents")]);
        let body = build_request_body(&req, false);
        assert_eq!(body.messages[0].role, "tool");
        assert_eq!(body.messages[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parses_a_non_streaming_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hello there"}}]}"#;
        let resp = parse_response(body).unwrap();
        assert_eq!(resp.content, "hello there");
        assert!(resp.tool_calls.is_empty());
    }

    #[test]
    fn parses_tool_calls_out_of_a_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":null,
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"main.rs\"}"}}]
        }}]}"#;
        let resp = parse_response(body).unwrap();
        assert_eq!(resp.content, "");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "read_file");
        assert_eq!(resp.tool_calls[0].arguments, json!({"path": "main.rs"}));
    }

    #[test]
    fn response_with_no_choices_is_an_internal_error() {
        let body = r#"{"choices":[]}"#;
        assert!(parse_response(body).is_err());
    }

    #[test]
    fn parses_a_stream_delta_chunk() {
        let payload = r#"{"choices":[{"delta":{"content":"Hel"}}]}"#;
        let chunk = parse_stream_payload(payload).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hel");
        assert!(!chunk.is_final);
    }

    #[test]
    fn stream_chunk_with_no_content_delta_yields_nothing() {
        let payload = r#"{"choices":[{"delta":{}}]}"#;
        assert!(parse_stream_payload(payload).unwrap().is_none());
    }
}
