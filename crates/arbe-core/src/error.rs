use std::time::Duration;

use thiserror::Error;

/// Every user-facing error carries a concise reason, a likely fix, and an
/// optional pointer to a verbose trace (harness spec §6).
pub trait UserFacing {
    fn reason(&self) -> String;
    fn likely_fix(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("authentication failed for provider: {0}")]
    Auth(String),
    /// A signed-in account (`arbeharness login`) can't be used: not signed
    /// in, session expired, not entitled, ... The message says what to do.
    #[error("{0}")]
    SignIn(String),
    #[error("rate limited by provider: {message}")]
    RateLimit {
        message: String,
        /// How long the provider asked us to wait (`Retry-After`), if it said.
        retry_after: Option<Duration>,
    },
    /// The provider is temporarily at capacity (e.g. HTTP 529/503); safe to retry.
    #[error("provider is overloaded: {0}")]
    Overloaded(String),
    #[error("provider request timed out: {0}")]
    Timeout(String),
    /// Couldn't connect at all (connection refused, DNS failure). Not
    /// retried: this almost always means a wrong base URL or a local
    /// server (Ollama) that isn't running, and backing off just delays
    /// telling the user.
    #[error("could not reach provider: {0}")]
    Unreachable(String),
    /// The connection failed after it was established (reset, closed
    /// mid-request). Transient; safe to retry.
    #[error("network error talking to provider: {0}")]
    Network(String),
    /// The request's prompt doesn't fit the model's context window.
    #[error("request exceeds the model's context window: {0}")]
    ContextLengthExceeded(String),
    #[error("invalid request sent to provider: {0}")]
    InvalidRequest(String),
    /// The request was cancelled by the harness before it completed.
    #[error("provider request was cancelled")]
    Cancelled,
    #[error("internal provider error: {0}")]
    Internal(String),
}

impl ProviderError {
    pub fn rate_limit(message: impl Into<String>) -> Self {
        Self::RateLimit {
            message: message.into(),
            retry_after: None,
        }
    }

    /// Whether retrying the same request later could succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimit { .. } | Self::Overloaded(_) | Self::Timeout(_) | Self::Network(_)
        )
    }
}

impl UserFacing for ProviderError {
    fn reason(&self) -> String {
        self.to_string()
    }

    fn likely_fix(&self) -> Option<String> {
        Some(
            match self {
                // The message already says what to do (log in again, ...).
                Self::SignIn(_) => return None,
                Self::Auth(_) => {
                    "check the provider's API key (e.g. OPENAI_API_KEY) is set and valid"
                }
                Self::RateLimit { .. } | Self::Overloaded(_) => {
                    "wait a moment and retry, or switch to a different model/provider"
                }
                Self::Timeout(_) | Self::Network(_) => {
                    "check network connectivity and that the provider endpoint is reachable"
                }
                Self::Unreachable(_) => {
                    "check the base URL, and that the server is running (for Ollama: `ollama serve`)"
                }
                Self::ContextLengthExceeded(_) => {
                    "lower the context budget or compact the conversation"
                }
                Self::InvalidRequest(_) => {
                    "check the model name and that it supports the requested features"
                }
                Self::Cancelled => return None,
                Self::Internal(_) => "retry; if it persists, check the provider's status page",
            }
            .to_string(),
        )
    }
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("tool invocation failed validation: {0}")]
    Validation(String),
    #[error("tool invocation was denied approval")]
    ApprovalDenied,
    #[error("tool execution failed at runtime: {0}")]
    RuntimeFailure(String),
    #[error("tool execution timed out")]
    Timeout,
    #[error("tool execution was cancelled")]
    Cancelled,
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("memory store unavailable: {0}")]
    StoreUnavailable(String),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid config schema: {0}")]
    InvalidSchema(String),
    #[error("missing required config value: {0}")]
    MissingValue(String),
    #[error("conflicting config values: {0}")]
    Conflict(String),
}

/// A hook's own failure. Timeouts and panics are caught by the hook
/// registry around the hook, so a hook only ever reports this.
#[derive(Debug, Error)]
pub enum HookError {
    #[error("hook violated its contract: {0}")]
    ContractViolation(String),
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Hook(#[from] HookError),
    /// A harness invariant was violated (e.g. an illegal agent-loop phase
    /// transition). Indicates a bug in the harness, not in user input,
    /// config, or a provider — surfaced as an error instead of a panic so
    /// one bad turn can't take down the process.
    #[error("internal harness error: {0}")]
    Internal(String),
    /// Reading or writing the harness's own files failed (sessions under
    /// the harness home): a disk or permission problem, not a harness bug.
    #[error("storage error: {0}")]
    Storage(String),
    /// The turn was cancelled (user interrupt or shutdown).
    #[error("cancelled")]
    Cancelled,
    /// Another turn is already running in this session.
    #[error("a turn is already in progress")]
    Busy,
}

impl UserFacing for ToolError {
    fn reason(&self) -> String {
        self.to_string()
    }

    fn likely_fix(&self) -> Option<String> {
        match self {
            Self::Validation(_) => Some("check the tool name and argument shape".to_string()),
            Self::Timeout => Some("raise the tool's timeout or narrow what it does".to_string()),
            Self::ApprovalDenied | Self::RuntimeFailure(_) | Self::Cancelled => None,
        }
    }
}

impl UserFacing for ConfigError {
    fn reason(&self) -> String {
        self.to_string()
    }

    fn likely_fix(&self) -> Option<String> {
        Some("fix the named value in config.toml or the matching environment variable".to_string())
    }
}

impl UserFacing for HarnessError {
    fn reason(&self) -> String {
        self.to_string()
    }

    fn likely_fix(&self) -> Option<String> {
        match self {
            Self::Provider(e) => e.likely_fix(),
            Self::Tool(e) => e.likely_fix(),
            Self::Config(e) => e.likely_fix(),
            Self::Internal(_) => Some("this is a harness bug; please report it".to_string()),
            Self::Storage(_) => Some(
                "check that the harness home (~/.arbe, or --dev-home) is readable and writable"
                    .to_string(),
            ),
            Self::Busy => Some("wait for the current turn to finish, or cancel it".to_string()),
            Self::Memory(_) | Self::Hook(_) | Self::Cancelled => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_provider_errors_are_retryable() {
        assert!(ProviderError::rate_limit("slow down").is_retryable());
        assert!(ProviderError::Overloaded("busy".into()).is_retryable());
        assert!(ProviderError::Timeout("t".into()).is_retryable());
        assert!(ProviderError::Network("reset".into()).is_retryable());
        assert!(!ProviderError::Unreachable("refused".into()).is_retryable());
        assert!(!ProviderError::Auth("bad".into()).is_retryable());
        assert!(!ProviderError::InvalidRequest("bad".into()).is_retryable());
        assert!(!ProviderError::Cancelled.is_retryable());
    }

    #[test]
    fn harness_errors_delegate_their_likely_fix() {
        let err = HarnessError::Provider(ProviderError::Auth("401".into()));
        assert!(err.likely_fix().unwrap().contains("API key"));
        assert!(HarnessError::Cancelled.likely_fix().is_none());
        // A sign-in error's message is its own fix; no API-key advice.
        let signin = ProviderError::SignIn("not signed in. Run `arbeharness login grok`.".into());
        assert_eq!(
            signin.to_string(),
            "not signed in. Run `arbeharness login grok`."
        );
        assert!(signin.likely_fix().is_none());
        assert!(!signin.is_retryable());
        assert!(
            HarnessError::Storage("disk full".into())
                .likely_fix()
                .unwrap()
                .contains("writable")
        );
    }
}
