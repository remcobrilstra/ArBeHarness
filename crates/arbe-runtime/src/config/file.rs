//! The `config.toml` schema. Every table rejects unknown keys, so a typo
//! is an error that names the file and line rather than a setting that
//! silently does nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

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
    pub mcp: Option<McpSection>,
    pub skills: Option<SkillsSection>,
    pub subagents: Option<SubagentsSection>,
    pub web: Option<WebSection>,
    /// Tools the model may use, by name (a trailing `*` matches a prefix,
    /// e.g. `"github__*"`). Absent = every registered tool.
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
    /// Project directories whose own `.arbe/config.toml` may change
    /// security-sensitive settings (global config only).
    pub trusted_projects: Option<Vec<PathBuf>>,
}

impl Layer {
    /// Removes the settings an untrusted project config must not control,
    /// here and in its profiles, and names what was removed:
    /// - where requests (and the API key) go: `provider.base_url`,
    ///   `provider.api_key_env`, `provider.headers`;
    /// - how tool calls are approved: `[approval]`;
    /// - programs started automatically: `[mcp]`;
    /// - trust itself: `trusted_projects`.
    pub fn strip_sensitive(&mut self) -> Vec<&'static str> {
        let mut removed = Vec::new();
        if let Some(provider) = &mut self.provider {
            if provider.base_url.take().is_some() {
                removed.push("provider.base_url");
            }
            if provider.api_key_env.take().is_some() {
                removed.push("provider.api_key_env");
            }
            if provider.api_key_command.take().is_some() {
                removed.push("provider.api_key_command");
            }
            if provider.headers.take().is_some() {
                removed.push("provider.headers");
            }
        }
        if self.approval.take().is_some() {
            removed.push("approval");
        }
        if self.mcp.take().is_some() {
            removed.push("mcp servers");
        }
        // Where search queries (and a key) go.
        if self.web.take().is_some() {
            removed.push("web");
        }
        if let Some(hooks) = &mut self.hooks
            && hooks.commands.take().is_some()
        {
            removed.push("hook commands");
        }
        if self.trusted_projects.take().is_some() {
            removed.push("trusted_projects");
        }
        for profile in self.profiles.values_mut() {
            removed.extend(profile.strip_sensitive());
        }
        removed.sort_unstable();
        removed.dedup();
        removed
    }
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
    /// A command that prints the API key (e.g. a password manager's CLI).
    pub api_key_command: Option<String>,
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
pub struct SkillsSection {
    /// `"on_demand"` (default) or `"always"`.
    pub mode: Option<String>,
}

/// `[web]`: web tools.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    pub search: Option<WebSearchSection>,
}

/// `[web.search]`: the service behind `web_search`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchSection {
    /// `brave`, `tavily` or `searxng`.
    pub backend: Option<String>,
    /// Environment variable holding the service's API key.
    pub api_key_env: Option<String>,
    /// The service's address (required for `searxng`).
    pub base_url: Option<String>,
}

/// `[subagents]`: the `task` tool.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentsSection {
    /// How deep subagents may nest: 1 (default) lets the agent start
    /// subagents that can't start their own; 0 turns the `task` tool off.
    pub max_depth: Option<u32>,
    /// Subagents running at once, across the whole session (default 4).
    pub max_concurrent: Option<usize>,
}

/// `[mcp.servers.<name>]` tables.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSection {
    #[serde(default)]
    pub servers: BTreeMap<String, arbe_mcp::McpServerSettings>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksSection {
    pub timeout_ms: Option<u64>,
    /// `[[hooks.commands]]`: shell commands run at a lifecycle phase.
    pub commands: Option<Vec<CommandHookEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandHookEntry {
    /// e.g. `"before_tool_execute"`.
    pub phase: String,
    pub command: String,
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

            [mcp.servers.github]
            command = "npx"
            args = ["-y", "@modelcontextprotocol/server-github"]

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
        assert_eq!(
            layer.mcp.unwrap().servers["github"].command.as_deref(),
            Some("npx")
        );
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
    fn stripping_removes_sensitive_settings_everywhere_and_keeps_the_rest() {
        let mut layer = parse(
            r#"
            tools = ["read_file"]
            [provider]
            model = "gpt-5"
            base_url = "https://evil.example"
            [approval]
            mode = "denylist_block"
            [mcp.servers.x]
            command = "curl"
            [profiles.p.provider]
            api_key_env = "OPENAI_API_KEY"
            "#,
        )
        .unwrap();
        let removed = layer.strip_sensitive();
        assert_eq!(
            removed,
            vec![
                "approval",
                "mcp servers",
                "provider.api_key_env",
                "provider.base_url"
            ]
        );
        assert!(layer.approval.is_none() && layer.mcp.is_none());
        assert_eq!(
            layer.provider.as_ref().unwrap().model.as_deref(),
            Some("gpt-5")
        );
        assert!(layer.provider.unwrap().base_url.is_none());
        assert!(
            layer.profiles["p"]
                .provider
                .as_ref()
                .unwrap()
                .api_key_env
                .is_none()
        );
        assert_eq!(layer.tools.unwrap(), vec!["read_file"]);
    }

    #[test]
    fn an_empty_file_is_an_empty_layer() {
        let layer = parse("").unwrap();
        assert!(layer.provider.is_none());
        assert!(layer.profiles.is_empty());
    }
}
