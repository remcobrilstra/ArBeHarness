//! Known models → capabilities (v2 plan P2.6).
//!
//! Adapters answer `ModelProvider::capabilities(model)` from here, so the
//! context budget, vision/thinking support, etc. reflect the actual model
//! rather than one hardcoded value per provider. Entries match by longest
//! model-name prefix, so dated snapshots (`gpt-4o-2024-08-06`,
//! `claude-sonnet-4-5-20250929`) resolve to their family.
//!
//! Values reflect the providers' published limits as of 2026-09. A model
//! that's missing, newer, or served with a different limit (e.g. a custom
//! Ollama `num_ctx`) can be corrected with an override rather than a code
//! change — see [`ModelCatalog::with_override`].

use std::collections::HashMap;

use crate::ModelCapabilities;

const fn caps(max_context_tokens: u64, vision: bool, thinking: bool) -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tool_calls: true,
        vision,
        thinking,
        prompt_caching: false,
        max_context_tokens,
    }
}

/// `(provider, model-name prefix, capabilities)`. Order doesn't matter;
/// the longest matching prefix wins.
const KNOWN: &[(&str, &str, ModelCapabilities)] = &[
    // OpenAI (prompt caching is automatic there, set below).
    ("openai", "gpt-5", caps(400_000, true, false)),
    ("openai", "gpt-4.1", caps(1_047_576, true, false)),
    ("openai", "gpt-4o", caps(128_000, true, false)),
    ("openai", "o1", caps(200_000, true, false)),
    ("openai", "o3", caps(200_000, true, false)),
    ("openai", "o4", caps(200_000, true, false)),
    // Anthropic: every current model has a 200k window; the older
    // 3.5-generation models lack extended thinking.
    ("anthropic", "claude-", caps(200_000, true, true)),
    ("anthropic", "claude-3-5", caps(200_000, true, false)),
    ("anthropic", "claude-3-haiku", caps(200_000, true, false)),
];

/// What a provider falls back to for a model the catalog doesn't know.
pub fn provider_default(provider: &str) -> ModelCapabilities {
    match provider {
        "openai" | "openai_compatible" => caps(128_000, true, false),
        "anthropic" => caps(200_000, true, true),
        // Matches the `num_ctx` the Ollama adapter requests.
        "ollama" => ModelCapabilities {
            vision: false,
            ..caps(crate::ollama::CONTEXT_WINDOW, false, false)
        },
        _ => caps(8_192, false, false),
    }
}

/// The built-in table plus per-model overrides.
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    overrides: HashMap<(String, String), ModelCapabilities>,
}

impl ModelCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pins exact capabilities for `model` on `provider` (exact name match,
    /// takes precedence over the built-in table).
    pub fn with_override(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
        capabilities: ModelCapabilities,
    ) -> Self {
        self.overrides
            .insert((provider.into(), model.into()), capabilities);
        self
    }

    pub fn lookup(&self, provider: &str, model: &str) -> ModelCapabilities {
        if let Some(caps) = self
            .overrides
            .get(&(provider.to_string(), model.to_string()))
        {
            return *caps;
        }
        let table_provider = match provider {
            // Compatible gateways often serve OpenAI's own models.
            "openai_compatible" => "openai",
            other => other,
        };
        let mut caps = KNOWN
            .iter()
            .filter(|(p, prefix, _)| *p == table_provider && model.starts_with(prefix))
            .max_by_key(|(_, prefix, _)| prefix.len())
            .map(|(_, _, caps)| *caps)
            .unwrap_or_else(|| provider_default(provider));
        caps.prompt_caching = matches!(provider, "openai" | "anthropic");
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins_including_dated_snapshots() {
        let catalog = ModelCatalog::new();
        assert_eq!(
            catalog.lookup("openai", "gpt-5-mini").max_context_tokens,
            400_000
        );
        assert_eq!(
            catalog
                .lookup("openai", "gpt-4o-2024-08-06")
                .max_context_tokens,
            128_000
        );
        assert_eq!(
            catalog.lookup("openai", "gpt-4.1-nano").max_context_tokens,
            1_047_576
        );
        assert!(
            catalog
                .lookup("anthropic", "claude-sonnet-4-5-20250929")
                .thinking
        );
        assert!(
            !catalog
                .lookup("anthropic", "claude-3-5-haiku-latest")
                .thinking
        );
    }

    #[test]
    fn unknown_models_fall_back_to_the_provider_default() {
        let catalog = ModelCatalog::new();
        assert_eq!(
            catalog.lookup("openai", "some-future-model"),
            ModelCapabilities {
                prompt_caching: true,
                ..provider_default("openai")
            }
        );
        let ollama = catalog.lookup("ollama", "qwen3");
        assert_eq!(ollama.max_context_tokens, crate::ollama::CONTEXT_WINDOW);
        assert!(!ollama.prompt_caching);
    }

    #[test]
    fn overrides_take_precedence() {
        let pinned = ModelCapabilities {
            max_context_tokens: 32_768,
            ..provider_default("ollama")
        };
        let catalog = ModelCatalog::new().with_override("ollama", "qwen3", pinned);
        assert_eq!(catalog.lookup("ollama", "qwen3"), pinned);
        assert_ne!(catalog.lookup("ollama", "qwen3:14b"), pinned);
    }

    #[test]
    fn compatible_gateways_resolve_openai_model_names() {
        let catalog = ModelCatalog::new();
        assert_eq!(
            catalog
                .lookup("openai_compatible", "gpt-4.1")
                .max_context_tokens,
            1_047_576
        );
        assert!(
            !catalog
                .lookup("openai_compatible", "gpt-4.1")
                .prompt_caching
        );
    }
}
