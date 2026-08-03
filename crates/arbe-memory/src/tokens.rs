/// Deterministic token estimate used for budget accounting (overall design
/// §5.3: "deterministic budget accounting"). This is a heuristic
/// (~4 chars/token, the same rule of thumb OpenAI publishes for English
/// text), not a real tokenizer — good enough to make truncation decisions
/// reproducible without depending on a provider-specific tokenizer.
pub fn estimate_tokens(text: &str) -> u64 {
    // Round up so even a short non-empty string costs at least one token.
    (text.chars().count() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

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
