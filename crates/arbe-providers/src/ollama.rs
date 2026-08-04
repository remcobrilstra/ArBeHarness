use arbe_core::{ProviderError, Role};
use async_trait::async_trait;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::error_map::{map_http_error, map_transport_error};
use crate::utf8_buffer::Utf8ChunkBuffer;
use crate::{ModelProvider, ModelRequest, ModelResponse, ProviderCapabilities, TokenChunk};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
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
        stream,
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponseLine {
    message: Option<ChatMessage>,
    done: bool,
}

fn parse_response(body: &str) -> Result<ModelResponse, ProviderError> {
    let parsed: ChatResponseLine = serde_json::from_str(body)
        .map_err(|e| ProviderError::Internal(format!("failed to parse Ollama response: {e}")))?;
    let content = parsed
        .message
        .map(|m| m.content)
        .ok_or_else(|| ProviderError::Internal("Ollama response had no message".to_string()))?;
    // Tool calling isn't wired up for Ollama yet (`capabilities().tool_calls`
    // is `false`, so `agent.rs` never sends `tools` here) — always empty.
    Ok(ModelResponse {
        content,
        tool_calls: Vec::new(),
    })
}

/// Returns `None` once the stream reports `done: true` with no further
/// content, `Some(chunk)` otherwise. Ollama frames its stream as
/// newline-delimited JSON objects rather than SSE, so no `SseDecoder` here.
fn parse_stream_line(line: &str) -> Result<Option<TokenChunk>, ProviderError> {
    let parsed: ChatResponseLine = serde_json::from_str(line)
        .map_err(|e| ProviderError::Internal(format!("failed to parse Ollama stream line: {e}")))?;
    let delta = parsed.message.map(|m| m.content).unwrap_or_default();
    if delta.is_empty() && parsed.done {
        return Ok(None);
    }
    Ok(Some(TokenChunk {
        delta,
        is_final: parsed.done,
    }))
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            tool_calls: false,
            json_mode: false,
            max_context_tokens: 8_192,
        }
    }

    async fn infer(&self, req: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = build_request_body(&req, false);
        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
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
            .post(format!("{}/api/chat", self.base_url))
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

            let mut buffer = String::new();
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
                buffer.push_str(&utf8_buf.push(&chunk));
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer.drain(..=newline_pos);
                    if line.is_empty() {
                        continue;
                    }
                    match parse_stream_line(&line) {
                        Ok(Some(token)) => yield Ok(token),
                        Ok(None) => break 'outer,
                        Err(e) => {
                            yield Err(e);
                            break 'outer;
                        }
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
            model: "llama3".to_string(),
            messages: vec![Message::new(Role::User, "hi")],
            temperature: 0.2,
            max_tokens: 100,
            tools: Vec::new(),
        };
        let body = build_request_body(&req, true);
        assert_eq!(body.model, "llama3");
        assert_eq!(body.messages[0].role, "user");
        assert!(body.stream);
    }

    #[test]
    fn parses_a_non_streaming_response() {
        let body = r#"{"message":{"role":"assistant","content":"hello"},"done":true}"#;
        let resp = parse_response(body).unwrap();
        assert_eq!(resp.content, "hello");
    }

    #[test]
    fn parses_a_stream_line_with_content() {
        let line = r#"{"message":{"role":"assistant","content":"Hel"},"done":false}"#;
        let chunk = parse_stream_line(line).unwrap().unwrap();
        assert_eq!(chunk.delta, "Hel");
        assert!(!chunk.is_final);
    }

    #[test]
    fn final_empty_line_ends_the_stream() {
        let line = r#"{"done":true}"#;
        assert!(parse_stream_line(line).unwrap().is_none());
    }
}
