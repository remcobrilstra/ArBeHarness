//! The `config.toml` schema. Every table rejects unknown keys, so a typo
//! is an error that names the file and line rather than a setting that
//! silently does nothing.

use std::collections::{BTreeMap, HashMap};

use arbe_core::ApprovalPolicyMode;
use serde::Deserialize;

/// One layer of settings: the top level of a config file, or one
/// `[profiles.<name>]` table. Every field is optional; a layer only
/// overrides what it sets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    /// Which profile to use (top level only).
    pub profile: Option<String>,
    pub provider: Option<ProviderSection>,
    pub generation: Option<GenerationSection>,
    pub context: Option<ContextSection>,
    #[serde(rename = "loop")]
    pub loop_: Option<LoopSection>,
    pub approval: Option<ApprovalSection>,
    pub hooks: Option<HooksSection>,
    /// Tools the model may use, by name. Absent = every registered tool.
    pub tools: Option<Vec<String>>,
    /// System prompt template: `"coding"`, `"general"`, or a path to a
    /// Markdown file (relative to the config file's directory).
    pub prompt: Option<String>,
    /// Named profiles (top level only).
    #[serde(default)]
    pub profiles: HashMap<String, Layer>,
    /// Per-model capability overrides (top level only).
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSection {
    pub name: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    /// Name of the environment variable holding the API key. The key
    /// itself never goes in a config file.
    pub api_key_env: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationSection {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u64>,
    pub thinking_budget_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSection {
    pub budget_tokens: Option<u64>,
    pub memory_strategy: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopSection {
    pub max_tool_rounds: Option<u32>,
    pub max_turn_tokens: Option<u64>,
    pub max_tool_output_chars: Option<usize>,
    pub max_retries: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSection {
    pub mode: Option<ApprovalPolicyMode>,
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub session_approval_covers_high_risk: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    pub timeout_ms: Option<u64>,
}

/// `[[models]]`: corrects the built-in model catalog for one model.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub provider: String,
    pub name: String,
    pub context_window: u64,
    pub tool_calls: Option<bool>,
    pub vision: Option<bool>,
    pub thinking: Option<bool>,
}

/// Parses one config file's text. The error message carries toml's
/// line/column pointer.
pub fn parse(text: &str) -> Result<Layer, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_section() {
        let layer = parse(
            r#"
            profile = "general"
            tools = ["read_file", "grep"]
            prompt = "general"

            [provider]
            name = "anthropic"
            model = "claude-sonnet-5"
            api_key_env = "WORK_ANTHROPIC_KEY"
            headers = { "X-Title" = "ArBe" }

            [generation]
            temperature = 0.5
            max_tokens = 8000
            thinking_budget_tokens = 4000

            [context]
            budget_tokens = 100000
            memory_strategy = "compact_summary"

            [loop]
            max_tool_rounds = 20
            max_turn_tokens = 500000
            max_tool_output_chars = 20000
            max_retries = 2

            [approval]
            mode = "allowlist_auto"
            allow = ["read_file"]
            deny = []
            session_approval_covers_high_risk = true

            [hooks]
            timeout_ms = 1000

            [profiles.review]
            tools = ["read_file", "grep", "glob"]
            [profiles.review.approval]
            mode = "allowlist_auto"
            allow = ["read_file", "grep", "glob"]

            [[models]]
            provider = "ollama"
            name = "qwen3"
            context_window = 32768
            "#,
        )
        .unwrap();
        assert_eq!(layer.profile.as_deref(), Some("general"));
        assert_eq!(
            layer.provider.unwrap().api_key_env.as_deref(),
            Some("WORK_ANTHROPIC_KEY")
        );
        assert_eq!(layer.loop_.unwrap().max_tool_rounds, Some(20));
        assert_eq!(
            layer.approval.unwrap().mode,
            Some(ApprovalPolicyMode::AllowlistAuto)
        );
        assert_eq!(layer.profiles["review"].tools.as_ref().unwrap().len(), 3);
        assert_eq!(layer.models[0].context_window, 32_768);
    }

    #[test]
    fn unknown_keys_are_errors_with_a_location() {
        let err = parse("[generation]\ntemprature = 0.5\n").unwrap_err();
        assert!(err.contains("temprature"), "{err}");
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn a_plain_api_key_is_refused() {
        let err = parse("[provider]\napi_key = \"sk-...\"\n").unwrap_err();
        assert!(err.contains("api_key"), "{err}");
    }

    #[test]
    fn an_empty_file_is_an_empty_layer() {
        let layer = parse("").unwrap();
        assert!(layer.provider.is_none());
        assert!(layer.profiles.is_empty());
    }
}
