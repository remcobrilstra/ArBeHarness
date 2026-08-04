//! Model provider abstraction (harness spec FR-2, overall design §4.2).
//! OpenAI and Ollama adapters ship in Phase 2 (implementation plan);
//! Anthropic/xAI/Mistral adapters follow the same `ModelProvider`
//! contract and can be added without touching call sites.

pub mod error_map;
pub mod ollama;
pub mod openai;
pub mod sse;
pub mod utf8_buffer;

pub use ollama::OllamaProvider;
pub use openai::OpenAiProvider;

use arbe_core::ProviderError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tool_calls: bool,
    pub json_mode: bool,
    pub max_context_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<arbe_core::Message>,
    pub temperature: f32,
    pub max_tokens: u64,
    /// Tools the model may call (empty when the caller isn't offering any,
    /// or the provider doesn't support it — see `ProviderCapabilities::tool_calls`).
    #[serde(default)]
    pub tools: Vec<arbe_core::ToolSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub content: String,
    /// Tool calls the model asked for. Non-empty only when `tools` was
    /// non-empty on the request and the model chose to use one.
    #[serde(default)]
    pub tool_calls: Vec<arbe_core::RequestedToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenChunk {
    pub delta: String,
    pub is_final: bool,
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    async fn infer(&self, req: ModelRequest) -> Result<ModelResponse, ProviderError>;
    async fn infer_stream(
        &self,
        req: ModelRequest,
    ) -> Result<
        Box<dyn futures_core::Stream<Item = Result<TokenChunk, ProviderError>> + Send + Unpin>,
        ProviderError,
    >;
}

/// Config-driven construction so swapping providers is a config change,
/// not a code change (overall design §7, implementation plan Phase 2 exit
/// criteria: "same prompt runs on both adapters by config switch only").
///
/// `api_key` is required for `"openai"` and ignored for `"ollama"`; per
/// NFR-4 it must come from env/config indirection, never a literal in code.
pub fn build_provider(
    provider_name: &str,
    api_key: Option<String>,
    base_url: Option<String>,
) -> Result<Box<dyn ModelProvider>, ProviderError> {
    match provider_name {
        "openai" => {
            let api_key = api_key.ok_or_else(|| {
                ProviderError::Auth("openai provider requires an api_key".to_string())
            })?;
            let mut provider = OpenAiProvider::new(api_key);
            if let Some(base_url) = base_url {
                provider = provider.with_base_url(base_url);
            }
            Ok(Box::new(provider))
        }
        "ollama" => {
            let mut provider = OllamaProvider::new();
            if let Some(base_url) = base_url {
                provider = provider.with_base_url(base_url);
            }
            Ok(Box::new(provider))
        }
        other => Err(ProviderError::InvalidRequest(format!(
            "unknown provider: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_openai_provider_with_api_key() {
        let provider = build_provider("openai", Some("sk-test".to_string()), None).unwrap();
        assert!(provider.capabilities().tool_calls);
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
        assert!(!provider.capabilities().tool_calls);
    }

    #[test]
    fn unknown_provider_name_is_invalid_request() {
        let Err(err) = build_provider("not-a-provider", None, None) else {
            panic!("expected an error");
        };
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
    }
}
