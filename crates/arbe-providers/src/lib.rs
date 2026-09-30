//! Model provider abstraction (harness spec FR-2, overall design §4.2).
//!
//! Every provider implements one method, [`ModelProvider::stream`], which
//! yields typed [`ProviderEvent`]s. Callers that don't need deltas use
//! [`infer`], which folds the same stream through a
//! [`ResponseAccumulator`] — there is no second, non-streaming code path to
//! keep in sync. Cancellation is handled here, once, by
//! [`http::cancellable`], not per adapter.

pub mod accumulator;
pub mod anthropic;
pub mod auth;
pub mod catalog;
pub mod error_map;
pub mod event;
mod grok;
pub mod http;
pub mod ollama;
pub mod openai;
pub mod registry;
pub mod retry;
pub mod sse;
mod text_tool_calls;
pub mod utf8_buffer;

pub use accumulator::{AccumulatedResponse, ResponseAccumulator};
pub use anthropic::AnthropicProvider;
pub use arbe_core::{StopReason, Usage};
pub use catalog::ModelCatalog;
pub use event::ProviderEvent;
pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;
pub use registry::{ProviderFactory, ProviderRegistry, ProviderSettings};
pub use retry::{RetryNotice, RetryPolicy, stream_with_retry};
pub use tokio_util::sync::CancellationToken;

use arbe_core::{Message, ProviderError, RequestedToolCall};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

/// What a specific model supports. Per model, not per provider: one
/// provider's models differ in context window, tool support, vision, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    pub vision: bool,
    pub thinking: bool,
    pub prompt_caching: bool,
    /// Total context window (prompt + output), in tokens.
    pub max_context_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    /// The conversation, including `Role::System` messages. Adapters whose
    /// API takes system instructions separately (Anthropic) hoist them.
    pub messages: Vec<Message>,
    pub temperature: f32,
    pub max_tokens: u64,
    /// Tools the model may call (empty when none are offered).
    #[serde(default)]
    pub tools: Vec<arbe_core::ToolSpec>,
    /// Extended-thinking budget, for models that support it
    /// (`ModelCapabilities::thinking`). `None` = no extended thinking;
    /// adapters for providers without the feature ignore it.
    #[serde(default)]
    pub thinking_budget_tokens: Option<u64>,
}

/// A provider's event stream. Ends after the provider's last event, or
/// with `Err(ProviderError::Cancelled)` if the request's token fires.
pub type ProviderStream = BoxStream<'static, Result<ProviderEvent, ProviderError>>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Stable provider id, as used in config (`"openai"`, `"ollama"`, ...).
    fn id(&self) -> &str;

    fn capabilities(&self, model: &str) -> ModelCapabilities;

    /// Starts a request and returns its event stream. Errors that happen
    /// before the first event (auth, bad request, rate limit, ...) are
    /// returned here; errors after that arrive as stream items.
    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError>;
}

/// A complete (non-streamed) model response.
#[derive(Debug, Clone)]
pub struct ModelResponse {
    /// The assistant message, as content blocks (text, thinking, tool uses).
    pub message: Message,
    pub usage: Usage,
    pub stop_reason: StopReason,
}

impl ModelResponse {
    pub fn text(&self) -> String {
        self.message.text()
    }

    pub fn tool_calls(&self) -> Vec<RequestedToolCall> {
        self.message.tool_uses()
    }
}

impl From<AccumulatedResponse> for ModelResponse {
    fn from(r: AccumulatedResponse) -> Self {
        Self {
            message: r.message,
            usage: r.usage,
            stop_reason: r.stop_reason,
        }
    }
}

/// A process-unique tool-call id, for providers that don't give calls one
/// (Ollama) or compatible servers that omit it. The harness needs an id to
/// thread each result back to its call; uniqueness across rounds and turns
/// means ids never collide within a conversation.
pub(crate) fn next_call_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!("{prefix}-call-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Runs a request to completion and returns the whole response — the
/// non-streaming convenience over [`ModelProvider::stream`].
pub async fn infer(
    provider: &dyn ModelProvider,
    req: ModelRequest,
    cancel: CancellationToken,
) -> Result<ModelResponse, ProviderError> {
    let mut stream = provider.stream(req, cancel).await?;
    let mut acc = ResponseAccumulator::new();
    while let Some(event) = stream.next().await {
        acc.push(event?);
    }
    Ok(acc.finish().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ContentBlock, Role};

    fn build_provider(
        provider_name: &str,
        api_key: Option<String>,
        base_url: Option<String>,
    ) -> Result<Box<dyn ModelProvider>, ProviderError> {
        ProviderRegistry::with_builtins().build(
            provider_name,
            ProviderSettings {
                api_key,
                base_url,
                ..Default::default()
            },
        )
    }

    #[test]
    fn builds_openai_provider_with_api_key() {
        let provider = build_provider("openai", Some("sk-test".to_string()), None).unwrap();
        assert_eq!(provider.id(), "openai");
        assert!(provider.capabilities("gpt-5").tool_calls);
    }

    #[test]
    fn builds_anthropic_provider_with_api_key_and_requires_one() {
        let provider = build_provider("anthropic", Some("sk-ant".to_string()), None).unwrap();
        assert_eq!(provider.id(), "anthropic");
        assert!(provider.capabilities("claude-sonnet-5").thinking);
        assert!(matches!(
            build_provider("anthropic", None, None),
            Err(ProviderError::Auth(_))
        ));
    }

    #[test]
    fn openai_without_api_key_is_an_auth_error() {
        let Err(err) = build_provider("openai", None, None) else {
            panic!("expected an error");
        };
        assert!(matches!(err, ProviderError::Auth(_)));
    }

    #[test]
    fn builds_ollama_provider_without_api_key() {
        let provider = build_provider("ollama", None, None).unwrap();
        assert_eq!(provider.id(), "ollama");
        assert!(provider.capabilities("llama3.1").tool_calls);
    }

    #[test]
    fn unknown_provider_name_is_invalid_request() {
        let Err(err) = build_provider("not-a-provider", None, None) else {
            panic!("expected an error");
        };
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }

    /// A provider that replays a fixed event script, for exercising
    /// `infer` and cancellation without any network.
    struct ScriptedProvider(Vec<ProviderEvent>);

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calls: true,
                vision: false,
                thinking: false,
                prompt_caching: false,
                max_context_tokens: 8_000,
            }
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            let events = self.0.clone().into_iter().map(Ok);
            Ok(http::cancellable(
                futures_util::stream::iter(events),
                cancel,
            ))
        }
    }

    fn request() -> ModelRequest {
        ModelRequest {
            model: "m".into(),
            messages: vec![Message::new(Role::User, "hi")],
            temperature: 0.0,
            max_tokens: 10,
            tools: vec![],
            thinking_budget_tokens: None,
        }
    }

    #[tokio::test]
    async fn infer_collects_the_stream_into_one_response() {
        let provider = ScriptedProvider(vec![
            ProviderEvent::TextDelta("hel".into()),
            ProviderEvent::TextDelta("lo".into()),
            ProviderEvent::Usage(Usage {
                input_tokens: 3,
                output_tokens: 2,
                ..Default::default()
            }),
            ProviderEvent::Stop(StopReason::EndTurn),
        ]);
        let resp = infer(&provider, request(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resp.text(), "hello");
        assert_eq!(resp.message.content, vec![ContentBlock::text("hello")]);
        assert_eq!(resp.usage.total_tokens(), 5);
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
    }

    #[tokio::test]
    async fn infer_on_an_already_cancelled_token_fails_with_cancelled() {
        let provider = ScriptedProvider(vec![ProviderEvent::TextDelta("never".into())]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = infer(&provider, request(), cancel).await.unwrap_err();
        assert!(matches!(err, ProviderError::Cancelled));
    }
}
