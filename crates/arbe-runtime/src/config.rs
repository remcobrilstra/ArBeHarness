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
    pub context_budget_tokens: u64,
    /// `"truncation"` or `"compact_summary"` (harness spec FR-5).
    pub memory_strategy: String,
    pub policy_mode: ApprovalPolicyMode,
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
    pub hook_timeout_ms: u64,
    pub system_instructions: Vec<String>,
}

impl RuntimeConfig {
    /// Reasonable defaults, overridable via env vars so the TUI is usable
    /// without editing code: `ARBE_PROVIDER` (default `ollama`),
    /// `ARBE_MODEL` (default `llama3`), `ARBE_BASE_URL`, and `OPENAI_API_KEY`
    /// (only consulted when `ARBE_PROVIDER=openai` — NFR-4: keys come from
    /// env, never a literal in code).
    pub fn from_env() -> Self {
        let provider_name = std::env::var("ARBE_PROVIDER").unwrap_or_else(|_| "ollama".to_string());
        let default_model = if provider_name == "openai" {
            "gpt-5-mini"
        } else {
            "llama3"
        };
        Self {
            profile: "default".to_string(),
            model: std::env::var("ARBE_MODEL").unwrap_or_else(|_| default_model.to_string()),
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            base_url: std::env::var("ARBE_BASE_URL").ok(),
            temperature: 0.2,
            max_tokens: 4096,
            context_budget_tokens: 8_000,
            memory_strategy: "truncation".to_string(),
            policy_mode: ApprovalPolicyMode::AlwaysPrompt,
            allowlist: Vec::new(),
            denylist: Vec::new(),
            hook_timeout_ms: 500,
            system_instructions: vec![
                "You are ArBeHarness, a terse and helpful coding assistant.".to_string(),
            ],
            provider_name,
        }
    }
}
