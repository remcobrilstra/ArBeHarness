use arbe_core::{ProviderError, Role};
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::error_map::{map_http_error, map_transport_error};
use crate::sse::{SseDecoder, SseItem};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
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
    max_tokens: u64,
    stream: bool,
}

fn build_request_body(req: &ModelRequest, stream: bool) -> ChatRequest {
    ChatRequest {
        model: req.model.clone(),
        messages: req
            .messages
            .iter()
            .map(|m| ChatMessage {
                role: role_str(m.role).to_string(),
                content: m.content.clone(),
            })
            .collect(),
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        stream,
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

fn parse_response(body: &str) -> Result<ModelResponse, ProviderError> {
    let parsed: ChatResponse = serde_json::from_str(body)
        .map_err(|e| ProviderError::Internal(format!("failed to parse OpenAI response: {e}")))?;
    let content = parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| ProviderError::Internal("OpenAI response had no choices".to_string()))?;
    Ok(ModelResponse { content })
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
            let mut bytes_stream = response.bytes_stream();
            'outer: while let Some(chunk) = bytes_stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(map_transport_error(e));
                        break;
                    }
                };
                let text = String::from_utf8_lossy(&chunk);
                for item in decoder.push(&text) {
                    match item {
                        SseItem::Done => break 'outer,
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
    use arbe_core::Message;

    #[test]
    fn builds_request_body_with_mapped_roles() {
        let req = ModelRequest {
            model: "gpt-5".to_string(),
            messages: vec![
                Message::new(Role::System, "be terse"),
                Message::new(Role::User, "hi"),
            ],
            temperature: 0.2,
            max_tokens: 100,
        };
        let body = build_request_body(&req, false);
        assert_eq!(body.model, "gpt-5");
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
        assert!(!body.stream);
    }

    #[test]
    fn parses_a_non_streaming_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hello there"}}]}"#;
        let resp = parse_response(body).unwrap();
        assert_eq!(resp.content, "hello there");
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
