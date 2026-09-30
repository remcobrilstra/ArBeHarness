use arbe_core::{ContentBlock, Message};

/// Flat estimate for one image. Real image cost depends on resolution and
/// provider (roughly 85-1600 tokens); budgeting the upper end keeps an
/// image-heavy context from overflowing.
pub const IMAGE_TOKEN_ESTIMATE: u64 = 1_600;

/// Deterministic token estimate used for budget accounting (overall design
/// §5.3: "deterministic budget accounting"). This is a heuristic
/// (~4 chars/token, the same rule of thumb OpenAI publishes for English
/// text), not a real tokenizer — good enough to make truncation decisions
/// reproducible without depending on a provider-specific tokenizer.
pub fn estimate_tokens(text: &str) -> u64 {
    // Round up so even a short non-empty string costs at least one token.
    (text.chars().count() as u64).div_ceil(4)
}

/// [`estimate_tokens`] over every block of a message: text, thinking,
/// tool names + JSON arguments, tool results, and a flat
/// [`IMAGE_TOKEN_ESTIMATE`] per image.
pub fn estimate_message_tokens(message: &Message) -> u64 {
    message.content.iter().map(estimate_block_tokens).sum()
}

pub(crate) fn estimate_block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => estimate_tokens(text),
        ContentBlock::Image { .. } => IMAGE_TOKEN_ESTIMATE,
        ContentBlock::ToolUse { name, input, .. } => {
            estimate_tokens(name) + estimate_json_tokens(input)
        }
        ContentBlock::ToolResult { content, .. } => content.iter().map(estimate_block_tokens).sum(),
        ContentBlock::Opaque { data, .. } => estimate_json_tokens(data),
    }
}

/// [`estimate_tokens`] of `value`'s compact JSON text, counted while
/// serializing instead of building the string: estimates run several
/// times per request over the whole history, and tool-call arguments
/// (whole files, for `write_file`) are its bulk.
fn estimate_json_tokens(value: &serde_json::Value) -> u64 {
    /// Counts UTF-8 characters written to it (bytes that don't continue a
    /// multi-byte character).
    struct CharCounter(u64);
    impl std::io::Write for CharCounter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.iter().filter(|b| (**b & 0xC0) != 0x80).count() as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = CharCounter(0);
    // Writing to a counter can't fail, and a `Value` always serializes.
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0.div_ceil(4)
}

/// Learns how far [`estimate_tokens`]'s ~4-chars/token heuristic is off
/// for the current model and content, from the input token counts the
/// provider actually reports (v2 plan P2.7). The ratio also absorbs what
/// the estimate never sees — per-message formatting overhead and the
/// tokenizer's real density for this content (code and JSON run denser
/// than the heuristic) — so budgeting against the calibrated estimate
/// keeps real requests inside the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenCalibration {
    factor: f64,
    /// Whether any request has been observed yet.
    observed: bool,
}

impl Default for TokenCalibration {
    fn default() -> Self {
        Self {
            factor: 1.0,
            observed: false,
        }
    }
}

impl TokenCalibration {
    /// Bounds on the learned factor: a single odd response (e.g. a huge
    /// cached prefix) can't swing budgeting wildly.
    const MIN: f64 = 0.5;
    const MAX: f64 = 3.0;
    /// Weight of each new observation (exponential moving average).
    const ALPHA: f64 = 0.3;

    pub fn factor(&self) -> f64 {
        self.factor
    }

    /// Records one request: its estimated prompt size vs. the provider's
    /// reported input tokens (including cached ones). The first observation
    /// is taken as is — the default of 1.0 is a guess, not evidence — and
    /// later ones are averaged in.
    pub fn observe(&mut self, estimated: u64, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let ratio = (actual as f64 / estimated as f64).clamp(Self::MIN, Self::MAX);
        self.factor = if self.observed {
            (1.0 - Self::ALPHA) * self.factor + Self::ALPHA * ratio
        } else {
            ratio
        };
        self.observed = true;
    }

    /// An estimate corrected toward real token counts.
    pub fn calibrate(&self, estimated: u64) -> u64 {
        (estimated as f64 * self.factor).round() as u64
    }

    /// A real-token budget expressed in estimator units, for code that
    /// budgets with `estimate_tokens`.
    pub fn budget_in_estimate_units(&self, budget: u64) -> u64 {
        (budget as f64 / self.factor).floor() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ImageSource, RequestedToolCall, Role};

    #[test]
    fn json_is_counted_like_its_text_without_building_it() {
        for value in [
            serde_json::json!({"path": "src/main.rs", "content": "fn main() {}\n"}),
            serde_json::json!({"text": "café 🎉 — ünïcode", "n": [1, 2.5, null, true]}),
            serde_json::json!("plain"),
        ] {
            assert_eq!(
                estimate_json_tokens(&value),
                estimate_tokens(&value.to_string())
            );
        }
    }
    use serde_json::json;

    #[test]
    fn calibration_moves_toward_observed_ratios_within_bounds() {
        let mut cal = TokenCalibration::default();
        assert_eq!(cal.calibrate(1_000), 1_000);
        // The first observation replaces the default outright...
        cal.observe(1_000, 1_300);
        assert!((cal.factor() - 1.3).abs() < 1e-9);
        assert_eq!(cal.calibrate(1_000), 1_300);
        assert_eq!(cal.budget_in_estimate_units(1_300), 1_000);
        // ...later ones are averaged in.
        cal.observe(1_000, 2_300);
        assert!((cal.factor() - 1.6).abs() < 1e-9);
        for _ in 0..50 {
            cal.observe(10, 1_000_000);
        }
        assert!((cal.factor() - 3.0).abs() < 1e-6);
        // Nothing to learn from an empty request or missing usage.
        let before = cal;
        cal.observe(0, 50);
        cal.observe(50, 0);
        assert_eq!(cal, before);
    }

    #[test]
    fn message_estimate_counts_every_block_kind() {
        let text = Message::new(Role::User, "abcdefgh");
        assert_eq!(estimate_message_tokens(&text), 2);

        let call = Message::assistant_tool_calls(vec![RequestedToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: json!({"p": "x"}),
        }]);
        // "read" = 1, "{\"p\":\"x\"}" (9 chars) = 3
        assert_eq!(estimate_message_tokens(&call), 4);

        let result = Message::tool_result("c", "abcd");
        assert_eq!(estimate_message_tokens(&result), 1);

        let image = Message::with_blocks(
            Role::User,
            vec![ContentBlock::Image {
                source: ImageSource::Url { url: "u".into() },
                media_type: "image/png".into(),
            }],
        );
        assert_eq!(estimate_message_tokens(&image), IMAGE_TOKEN_ESTIMATE);
    }

    #[test]
    fn empty_string_costs_nothing() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn short_string_costs_at_least_one_token() {
        assert_eq!(estimate_tokens("hi"), 1);
    }

    #[test]
    fn scales_roughly_with_length() {
        let short = estimate_tokens("hello");
        let long = estimate_tokens(&"hello ".repeat(100));
        assert!(long > short * 50);
    }
}
