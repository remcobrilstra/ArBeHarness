//! Provider construction by id (v2 plan P2.8).
//!
//! Adding a provider means registering a factory, not editing a central
//! `match` — and an embedder can register its own alongside the builtins.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arbe_core::ProviderError;

use crate::catalog::ModelCatalog;
use crate::grok::GrokSubscriptionProvider;
use crate::{AnthropicProvider, ModelProvider, OllamaProvider, OpenAiProvider};

/// Everything config can say about how to reach a provider.
#[derive(Debug, Clone, Default)]
pub struct ProviderSettings {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Sent on every request, e.g. a gateway's routing or attribution
    /// headers.
    pub extra_headers: Vec<(String, String)>,
    /// Per-model capability overrides on top of the built-in table.
    pub catalog: ModelCatalog,
    /// Where signed-in accounts are stored (`<home>/auth`, see
    /// [`crate::auth`]), for providers that sign in instead of taking an
    /// API key.
    pub auth_dir: Option<PathBuf>,
}

pub type ProviderFactory =
    Arc<dyn Fn(ProviderSettings) -> Result<Box<dyn ModelProvider>, ProviderError> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    factories: HashMap<String, ProviderFactory>,
}

fn require_key(provider: &str, settings: &ProviderSettings) -> Result<String, ProviderError> {
    settings
        .api_key
        .clone()
        .filter(|k| !k.is_empty())
        .ok_or_else(|| ProviderError::Auth(format!("{provider} provider requires an api_key")))
}

impl ProviderRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// `openai`, `openai_compatible`, `anthropic`, `ollama`, `grok_subscription`.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register("openai", |s: ProviderSettings| {
            let key = require_key("openai", &s)?;
            let mut p = OpenAiProvider::new(key)
                .with_headers(s.extra_headers)
                .with_catalog(s.catalog);
            if let Some(url) = s.base_url {
                p = p.with_base_url(url);
            }
            Ok(Box::new(p) as Box<dyn ModelProvider>)
        });
        registry.register("openai_compatible", |s: ProviderSettings| {
            // Local servers (vLLM, LM Studio, llama.cpp) usually need no
            // key, and there is no sensible default address.
            let url = s.base_url.clone().ok_or_else(|| {
                ProviderError::InvalidRequest(
                    "openai_compatible provider requires a base_url".to_string(),
                )
            })?;
            Ok(Box::new(
                OpenAiProvider::compatible(url, s.api_key)
                    .with_headers(s.extra_headers)
                    .with_catalog(s.catalog),
            ) as Box<dyn ModelProvider>)
        });
        registry.register("anthropic", |s: ProviderSettings| {
            let key = require_key("anthropic", &s)?;
            let mut p = AnthropicProvider::new(key)
                .with_headers(s.extra_headers)
                .with_catalog(s.catalog);
            if let Some(url) = s.base_url {
                p = p.with_base_url(url);
            }
            Ok(Box::new(p) as Box<dyn ModelProvider>)
        });
        registry.register("ollama", |s: ProviderSettings| {
            let mut p = OllamaProvider::new().with_catalog(s.catalog);
            if let Some(url) = s.base_url {
                p = p.with_base_url(url);
            }
            Ok(Box::new(p) as Box<dyn ModelProvider>)
        });
        registry.register("grok_subscription", |s: ProviderSettings| {
            let auth_dir = s.auth_dir.clone().ok_or_else(|| {
                ProviderError::InvalidRequest(
                    "grok_subscription needs a directory for its sign-in credential".into(),
                )
            })?;
            Ok(Box::new(GrokSubscriptionProvider::new(
                s.base_url,
                &auth_dir,
                s.extra_headers,
                s.catalog,
            )?) as Box<dyn ModelProvider>)
        });
        registry
    }

    pub fn register<F>(&mut self, id: impl Into<String>, factory: F)
    where
        F: Fn(ProviderSettings) -> Result<Box<dyn ModelProvider>, ProviderError>
            + Send
            + Sync
            + 'static,
    {
        self.factories.insert(id.into(), Arc::new(factory));
    }

    pub fn build(
        &self,
        id: &str,
        settings: ProviderSettings,
    ) -> Result<Box<dyn ModelProvider>, ProviderError> {
        let factory = self.factories.get(id).ok_or_else(|| {
            ProviderError::InvalidRequest(format!(
                "unknown provider: {id} (known: {})",
                self.ids().join(", ")
            ))
        })?;
        factory(settings)
    }

    /// Registered provider ids, sorted.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.factories.keys().cloned().collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_registered() {
        assert_eq!(
            ProviderRegistry::with_builtins().ids(),
            vec![
                "anthropic",
                "grok_subscription",
                "ollama",
                "openai",
                "openai_compatible"
            ]
        );
    }

    #[test]
    fn unknown_ids_list_the_known_ones() {
        let Err(err) = ProviderRegistry::with_builtins().build("nope", ProviderSettings::default())
        else {
            panic!("expected an error");
        };
        assert!(err.to_string().contains("openai_compatible"));
    }

    #[test]
    fn openai_compatible_needs_a_base_url_but_no_key() {
        let registry = ProviderRegistry::with_builtins();
        assert!(matches!(
            registry.build("openai_compatible", ProviderSettings::default()),
            Err(ProviderError::InvalidRequest(_))
        ));
        let provider = registry
            .build(
                "openai_compatible",
                ProviderSettings {
                    base_url: Some("http://localhost:8000/v1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(provider.id(), "openai_compatible");
    }

    #[test]
    fn the_catalog_in_settings_reaches_the_provider() {
        let pinned = crate::ModelCapabilities {
            max_context_tokens: 32_768,
            ..crate::catalog::provider_default("ollama")
        };
        let provider = ProviderRegistry::with_builtins()
            .build(
                "ollama",
                ProviderSettings {
                    catalog: ModelCatalog::new().with_override("ollama", "qwen3", pinned),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(provider.capabilities("qwen3").max_context_tokens, 32_768);
    }

    #[test]
    fn custom_providers_can_be_registered() {
        let mut registry = ProviderRegistry::new();
        registry.register("mine", |_s: ProviderSettings| {
            Ok(Box::new(OllamaProvider::new()) as Box<dyn ModelProvider>)
        });
        assert!(registry.build("mine", ProviderSettings::default()).is_ok());
    }
}
