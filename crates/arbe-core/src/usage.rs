use serde::{Deserialize, Serialize};

/// Token accounting reported by a provider for one inference call, or
/// summed over a turn/session. Counts a provider doesn't report stay 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Input tokens served from the provider's prompt cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Input tokens written to the provider's prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// A model's prices, in US dollars per million tokens (set by the user in
/// config; the harness ships no prices, since they change).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    /// For tokens served from the prompt cache (default: the input price).
    #[serde(default)]
    pub cache_read: Option<f64>,
    /// For tokens written to the prompt cache (default: the input price).
    #[serde(default)]
    pub cache_write: Option<f64>,
}

impl Pricing {
    /// What `usage` cost, in US dollars.
    pub fn cost(&self, usage: &Usage) -> f64 {
        let per_token = |price: f64, tokens: u64| price * tokens as f64 / 1_000_000.0;
        per_token(self.input, usage.input_tokens)
            + per_token(self.output, usage.output_tokens)
            + per_token(
                self.cache_read.unwrap_or(self.input),
                usage.cache_read_tokens,
            )
            + per_token(
                self.cache_write.unwrap_or(self.input),
                usage.cache_write_tokens,
            )
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
    }
}

/// Why a model stopped producing output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    EndTurn,
    /// The model stopped to have tools run.
    ToolUse,
    /// The output hit `max_tokens`.
    MaxTokens,
    StopSequence,
    /// The model declined to answer.
    Refusal,
    /// The harness cancelled the request (user interrupt, shutdown).
    Cancelled,
    /// The process stopped mid-turn (crash, kill); the turn was recovered
    /// from its in-flight log on the next resume.
    Interrupted,
    /// Loop guard: the turn used `max_tool_rounds` model<->tool rounds.
    ToolRoundLimit,
    /// Loop guard: the turn's token usage passed its configured ceiling.
    TurnTokenLimit,
    /// Loop guard: the model kept requesting the exact same tool call.
    RepeatedToolCall,
    /// A provider-specific reason the harness doesn't model.
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_prices_each_kind_of_token() {
        let pricing = Pricing {
            input: 3.0,
            output: 15.0,
            cache_read: Some(0.3),
            cache_write: None,
        };
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 2_000_000,
            cache_write_tokens: 1_000_000,
        };
        // 3 + 1.5 + 0.6 + 3 (cache writes at the input price)
        assert!((pricing.cost(&usage) - 8.1).abs() < 1e-9);
    }

    #[test]
    fn usage_adds_field_by_field() {
        let mut total = Usage {
            input_tokens: 10,
            output_tokens: 2,
            cache_read_tokens: 1,
            cache_write_tokens: 0,
        };
        total += Usage {
            input_tokens: 5,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 4,
        };
        assert_eq!(total.input_tokens, 15);
        assert_eq!(total.output_tokens, 5);
        assert_eq!(total.cache_read_tokens, 1);
        assert_eq!(total.cache_write_tokens, 4);
        assert_eq!(total.total_tokens(), 20);
    }

    #[test]
    fn stop_reason_round_trips() {
        for reason in [
            StopReason::EndTurn,
            StopReason::Other("content_filter".into()),
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(serde_json::from_str::<StopReason>(&json).unwrap(), reason);
        }
    }
}
