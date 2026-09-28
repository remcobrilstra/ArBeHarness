use std::path::PathBuf;

use arbe_core::ApprovalPolicyMode;

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
    /// without editing code: `ARBE_PROVIDER` (default `ollama`),
    /// `ARBE_MODEL` (default `llama3.1` — tool-capable; for OpenAI `gpt-5-mini`), `ARBE_BASE_URL`, `OPENAI_API_KEY`
    /// (only consulted when `ARBE_PROVIDER=openai` — NFR-4: keys come from
    /// env, never a literal in code), and `ARBE_WORKDIR` (default: the
    /// process's current directory) for the project the agent works on.
    /// Note this is separate from `ARBE_HOME`, which relocates the
    /// harness's *own* storage root (`~/.arbe/`) and is a dev/test-only
    /// knob — see `arbe_storage::paths`.
    pub fn from_env() -> Self {
        let provider_name = std::env::var("ARBE_PROVIDER").unwrap_or_else(|_| "ollama".to_string());
        let is_openai = provider_name == "openai";
        let default_model = if is_openai { "gpt-5-mini" } else { "llama3.1" };
        // OpenAI's newer reasoning-family models (o1/o3/gpt-5) reject any
        // temperature other than the default (1) with a 400 error; older
        // chat models tolerate a lower temperature fine, but 1 is a safe
        // default for both. `ARBE_TEMPERATURE` overrides this if a caller
        // knows their chosen model supports something else.
        let default_temperature = if is_openai { 1.0 } else { 0.2 };
        Self {
            profile: "default".to_string(),
            model: std::env::var("ARBE_MODEL").unwrap_or_else(|_| default_model.to_string()),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            base_url: std::env::var("ARBE_BASE_URL").ok(),
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
