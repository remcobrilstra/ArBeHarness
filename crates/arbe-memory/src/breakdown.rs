//! Measuring what a context is made of (v2 plan P5.7), in the same
//! estimator units as everything else in this crate. The runtime
//! calibrates the result before publishing it.

use arbe_core::{ContentBlock, Message, MessageTokens, Role, ToolSpec};

use crate::prune::is_stub;
use crate::tokens::{IMAGE_TOKEN_ESTIMATE, estimate_block_tokens, estimate_tokens};

/// Tokens in `messages`, by kind of content. Text is attributed by the
/// message's role (system text in the history is a harness notice).
pub fn measure_messages(messages: &[Message]) -> MessageTokens {
    let mut tokens = MessageTokens::default();
    for message in messages {
        for block in &message.content {
            let cost = estimate_block_tokens(block);
            match block {
                ContentBlock::Text { .. } | ContentBlock::Opaque { .. } => match message.role {
                    Role::User => tokens.user += cost,
                    Role::Assistant => tokens.assistant += cost,
                    Role::System => tokens.notices += cost,
                    Role::Tool => tokens.tool_results += cost,
                },
                ContentBlock::Thinking { .. } => tokens.thinking += cost,
                ContentBlock::Image { .. } => tokens.images += IMAGE_TOKEN_ESTIMATE,
                ContentBlock::ToolUse { .. } => tokens.tool_calls += cost,
                ContentBlock::ToolResult { .. } => tokens.tool_results += cost,
            }
        }
    }
    tokens
}

/// Turns in a stretch of history: a turn starts at each user message
/// (tool results travel as `Role::Tool`, so they don't count).
pub fn count_turns(messages: &[Message]) -> u64 {
    messages.iter().filter(|m| m.role == Role::User).count() as u64
}

/// Tool results in `messages` that were replaced by a pruning stub.
pub fn count_stubbed_results(messages: &[Message]) -> u64 {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|block| match block {
            ContentBlock::ToolResult { content, .. } => {
                matches!(content.as_slice(), [ContentBlock::Text { text }] if is_stub(text))
            }
            _ => false,
        })
        .count() as u64
}

/// Tool definitions as the model receives them: name, description and
/// JSON schema, per tool.
pub fn estimate_tool_specs(specs: &[ToolSpec]) -> u64 {
    specs
        .iter()
        .map(|spec| {
            estimate_tokens(&spec.name)
                + estimate_tokens(&spec.description)
                + estimate_tokens(&spec.parameters.to_string())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prune::prune_tool_results;
    use arbe_core::RequestedToolCall;
    use serde_json::json;

    fn a_turn(output: &str) -> Vec<Message> {
        vec![
            Message::new(Role::User, "read it"),
            Message::assistant_tool_calls(vec![RequestedToolCall {
                id: "c1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a.rs"}),
            }]),
            Message::tool_result("c1", output),
            Message::new(Role::Assistant, "done"),
        ]
    }

    #[test]
    fn each_kind_of_content_lands_in_its_own_bucket() {
        let mut messages = a_turn(&"x".repeat(400));
        messages.push(Message::new(Role::System, "[2 earlier messages omitted]"));
        let tokens = measure_messages(&messages);
        assert_eq!(tokens.user, estimate_tokens("read it"));
        assert_eq!(tokens.assistant, estimate_tokens("done"));
        assert_eq!(tokens.tool_results, 100);
        assert_eq!(
            tokens.tool_calls,
            estimate_tokens("read_file") + estimate_tokens(&json!({"path": "a.rs"}).to_string())
        );
        assert_eq!(
            tokens.notices,
            estimate_tokens("[2 earlier messages omitted]")
        );
        let whole: u64 = messages.iter().map(crate::estimate_message_tokens).sum();
        assert_eq!(tokens.total(), whole, "nothing is lost or counted twice");
    }

    #[test]
    fn turns_are_counted_by_user_message() {
        let mut messages = a_turn("a");
        messages.extend(a_turn("b"));
        assert_eq!(count_turns(&messages), 2);
    }

    #[test]
    fn stubbed_results_are_recognized() {
        let mut messages = a_turn(&"x".repeat(4_000));
        messages.extend(a_turn("short"));
        assert_eq!(count_stubbed_results(&messages), 0);
        assert_eq!(prune_tool_results(&mut messages, 10, 0), 1);
        assert_eq!(count_stubbed_results(&messages), 1);
    }

    #[test]
    fn tool_definitions_are_measured_whole() {
        let spec = ToolSpec {
            name: "grep".into(),
            description: "Search file contents".into(),
            parameters: json!({"type": "object", "properties": {"pattern": {"type": "string"}}}),
        };
        let one = estimate_tool_specs(std::slice::from_ref(&spec));
        assert!(one > estimate_tokens("Search file contents"));
        assert_eq!(estimate_tool_specs(&[spec.clone(), spec]), one * 2);
        assert_eq!(estimate_tool_specs(&[]), 0);
    }
}
