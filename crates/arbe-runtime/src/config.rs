use std::path::PathBuf;

use arbe_core::ApprovalPolicyMode;
use arbe_providers::RetryPolicy;

/// Runtime configuration for one `Agent` (overall design §7 config schema
/// draft). `from_env` provides sane, no-network-required defaults
/// (Ollama, always-prompt approval) so the TUI has something to run
/// against without requiring an API key.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub profile: String,
    pub provider_name: String,
    pub model: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Extra HTTP headers for every provider request, e.g. for a gateway
    /// (`ARBE_HTTP_HEADERS="Name: value; Other: value"`).
    pub extra_headers: Vec<(String, String)>,
    pub temperature: f32,
    pub max_tokens: u64,
    /// Total token budget for an assembled context. `None` (the default)
    /// derives it from the provider's context window minus `max_tokens`
    /// (see `effective_context_budget`), so a large-window model isn't
    /// artificially starved and a small one isn't overflowed.
    /// `ARBE_CONTEXT_BUDGET` overrides.
    pub context_budget_tokens: Option<u64>,
    /// `"truncation"` or `"compact_summary"` (harness spec FR-5).
    pub memory_strategy: String,
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
    pub hook_timeout_ms: u64,
    /// Whether "approve for session" also covers `RiskLevel::High` tools
    /// (e.g. `execute`). Off by default — see
    /// `arbe_tools::ApprovalContext::session_approval_covers_high_risk`.
    pub session_approval_covers_high_risk: bool,
    /// Maximum model<->tool round trips in one turn before the turn stops
    /// (`ARBE_MAX_TOOL_ROUNDS`, default 50). Real multi-step work routinely
    /// needs dozens of tool calls; this is a runaway guard, not a budget.
    pub max_tool_rounds: u32,
    /// Loop guard: stop the turn once its total token usage (all rounds)
    /// passes this. `None` (default) = no ceiling. `ARBE_MAX_TURN_TOKENS`.
    pub max_turn_tokens: Option<u64>,
    /// Longest tool result (in chars) sent back to the model; longer
    /// output keeps its head and tail with a marker in between.
    /// `ARBE_MAX_TOOL_OUTPUT_CHARS`, default 50 000 (~12k tokens).
    pub max_tool_output_chars: usize,
    /// Retry/backoff for transient provider failures. `ARBE_MAX_RETRIES`
    /// overrides the retry count (0 disables retrying).
    pub retry: RetryPolicy,
    /// Extended-thinking budget for models that support it (Anthropic).
    /// `None` (default) disables extended thinking.
    pub thinking_budget_tokens: Option<u64>,
    /// The directory the agent operates *in* — usually the repo it's
    /// working on. This is distinct from `ARBE_HOME`/`~/.arbe/`, which is
    /// where the harness's own persistent state (sessions, skills, memory)
    /// lives, not the target of the agent's work. File/execute tools will
    /// use this as their sandbox root once implemented; project-local
    /// skills/memory will key off it too.
    pub project_dir: PathBuf,
}

impl RuntimeConfig {
    /// Reasonable defaults, overridable via env vars so the TUI is usable
    /// without editing code: `ARBE_PROVIDER` (`ollama` default, `openai`,
    /// `anthropic`), `ARBE_MODEL` (per-provider default, see
    /// `default_model`), `ARBE_BASE_URL`, the provider's API key (see
    /// `api_key_from_env` — NFR-4: keys come from env, never a literal in
    /// code), `ARBE_THINKING_BUDGET`, and `ARBE_WORKDIR` (default: the
    /// process's current directory) for the project the agent works on.
    /// Note this is separate from `ARBE_HOME`, which relocates the
    /// harness's *own* storage root (`~/.arbe/`) and is a dev/test-only
    /// knob — see `arbe_storage::paths`.
    pub fn from_env() -> Self {
        let provider_name = std::env::var("ARBE_PROVIDER").unwrap_or_else(|_| "ollama".to_string());
        let is_openai = provider_name == "openai";
        // OpenAI's newer reasoning-family models (o1/o3/gpt-5) reject any
        // temperature other than the default (1) with a 400 error; older
        // chat models tolerate a lower temperature fine, but 1 is a safe
        // default for both. `ARBE_TEMPERATURE` overrides this if a caller
        // knows their chosen model supports something else.
        let default_temperature = if is_openai { 1.0 } else { 0.2 };
        Self {
            profile: "default".to_string(),
            model: std::env::var("ARBE_MODEL")
                .unwrap_or_else(|_| default_model(&provider_name).to_string()),
            api_key: api_key_from_env(&provider_name),
            base_url: std::env::var("ARBE_BASE_URL").ok(),
            extra_headers: std::env::var("ARBE_HTTP_HEADERS")
                .map(|v| parse_headers(&v))
                .unwrap_or_default(),
            temperature: env_parse("ARBE_TEMPERATURE").unwrap_or(default_temperature),
            max_tokens: 4096,
            context_budget_tokens: env_parse("ARBE_CONTEXT_BUDGET"),
            memory_strategy: "truncation".to_string(),
            policy_mode: ApprovalPolicyMode::AlwaysPrompt,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            hook_timeout_ms: 500,
            session_approval_covers_high_risk: false,
            max_tool_rounds: env_parse("ARBE_MAX_TOOL_ROUNDS").unwrap_or(DEFAULT_MAX_TOOL_ROUNDS),
            max_turn_tokens: env_parse("ARBE_MAX_TURN_TOKENS"),
            max_tool_output_chars: env_parse("ARBE_MAX_TOOL_OUTPUT_CHARS")
                .unwrap_or(DEFAULT_MAX_TOOL_OUTPUT_CHARS),
            retry: RetryPolicy {
                max_retries: env_parse("ARBE_MAX_RETRIES")
                    .unwrap_or(RetryPolicy::default().max_retries),
                ..RetryPolicy::default()
            },
            thinking_budget_tokens: env_parse("ARBE_THINKING_BUDGET"),
            provider_name,
            project_dir: std::env::var("ARBE_WORKDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        }
    }

    /// The context budget to actually use: the explicit
    /// `context_budget_tokens` if set, otherwise the provider's context
    /// window minus the output reservation (`max_tokens`). If `max_tokens`
    /// would eat the whole window, fall back to half the window rather
    /// than a zero budget.
    pub fn effective_context_budget(&self, provider_context_window: u64) -> u64 {
        self.context_budget_tokens.unwrap_or_else(|| {
            let remaining = provider_context_window.saturating_sub(self.max_tokens);
            if remaining == 0 {
                provider_context_window / 2
            } else {
                remaining
            }
        })
    }
}

const DEFAULT_MAX_TOOL_ROUNDS: u32 = 50;
const DEFAULT_MAX_TOOL_OUTPUT_CHARS: usize = 50_000;

/// Parses `"Name: value; Other: value"`. Entries without a `:` or with an
/// empty name are ignored rather than failing startup.
fn parse_headers(raw: &str) -> Vec<(String, String)> {
    raw.split(';')
        .filter_map(|entry| {
            let (name, value) = entry.split_once(':')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The model used when `ARBE_MODEL` isn't set.
fn default_model(provider: &str) -> &'static str {
    match provider {
        "openai" => "gpt-5-mini",
        "anthropic" => "claude-sonnet-5",
        // Tool-capable, unlike the original `llama3`.
        _ => "llama3.1",
    }
}

/// The provider's conventional key variable, falling back to the generic
/// `ARBE_API_KEY` (e.g. for an OpenAI-compatible gateway).
fn api_key_from_env(provider: &str) -> Option<String> {
    let specific = match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        _ => None,
    };
    specific
        .and_then(|var| std::env::var(var).ok())
        .or_else(|| std::env::var("ARBE_API_KEY").ok())
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(budget: Option<u64>, max_tokens: u64) -> RuntimeConfig {
        RuntimeConfig {
            context_budget_tokens: budget,
            max_tokens,
            ..RuntimeConfig::from_env()
        }
    }

    #[test]
    fn parses_header_lists_and_skips_malformed_entries() {
        assert_eq!(
            parse_headers("HTTP-Referer: https://x.dev; X-Title:ArBe ;junk; :empty"),
            vec![
                ("HTTP-Referer".to_string(), "https://x.dev".to_string()),
                ("X-Title".to_string(), "ArBe".to_string()),
            ]
        );
    }

    #[test]
    fn explicit_budget_wins() {
        assert_eq!(
            config(Some(1_000), 4_096).effective_context_budget(128_000),
            1_000
        );
    }

    #[test]
    fn derived_budget_reserves_room_for_output() {
        assert_eq!(
            config(None, 4_096).effective_context_budget(128_000),
            123_904
        );
    }

    #[test]
    fn derived_budget_never_collapses_to_zero() {
        assert_eq!(config(None, 10_000).effective_context_budget(8_192), 4_096);
    }
}
