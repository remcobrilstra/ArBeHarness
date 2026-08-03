//! Model provider abstraction (harness spec FR-2, overall design §4.2).
//! Concrete adapters (OpenAI, Anthropic, xAI, Mistral, Ollama) land in
//! Phase 2; this crate currently defines only the shared contract.

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelResponse {
    pub content: String,
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
