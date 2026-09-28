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

fn estimate_block_tokens(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Text { text } | ContentBlock::Thinking { text, .. } => estimate_tokens(text),
        ContentBlock::Image { .. } => IMAGE_TOKEN_ESTIMATE,
        ContentBlock::ToolUse { name, input, .. } => {
            estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => content.iter().map(estimate_block_tokens).sum(),
        ContentBlock::Opaque { data, .. } => estimate_tokens(&data.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ImageSource, RequestedToolCall, Role};
    use serde_json::json;

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
