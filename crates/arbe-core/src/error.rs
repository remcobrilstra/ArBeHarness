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
    #[error("rate limited by provider: {0}")]
    RateLimit(String),
    #[error("provider request timed out: {0}")]
    Timeout(String),
    #[error("invalid request sent to provider: {0}")]
    InvalidRequest(String),
    #[error("internal provider error: {0}")]
    Internal(String),
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
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("failed to parse memory content: {0}")]
    ParseFailure(String),
    #[error("context budget could not be satisfied: {0}")]
    BudgetFailure(String),
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

#[derive(Debug, Error)]
pub enum HookError {
    #[error("hook timed out")]
    Timeout,
    #[error("hook panicked: {0}")]
    Panic(String),
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
}
