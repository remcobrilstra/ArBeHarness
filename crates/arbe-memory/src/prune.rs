//! Tool-output pruning (v2 plan P5.3): the cheapest way to free context.
//!
//! Old tool results are usually the bulk of a long conversation and the
//! part the model needs least once it has acted on them. Replacing their
//! text with a one-line stub — oldest first, until the budget fits —
//! keeps every message (so tool calls and results stay paired and the
//! conversation still reads coherently) and only then, if that isn't
//! enough, do whole turns have to go.

use arbe_core::{ContentBlock, Message, Role};

use crate::tokens::estimate_message_tokens;

/// Results smaller than this aren't worth stubbing (the stub itself costs
/// ~15 tokens, and small results are often the most informative).
const MIN_PRUNABLE_TOKENS: u64 = 200;

fn stub(tokens: u64) -> String {
    format!(
        "[tool output omitted to save context (~{tokens} tokens); re-run the tool if it's needed again]"
    )
}

/// Stubs out tool results, oldest first, until the estimated size of
/// `messages` is at most `budget` or nothing prunable is left. The last
/// `protect_last` tool-result messages are never touched (e.g. the results
/// the model is about to read for the first time). Returns how many
/// results were stubbed.
pub fn prune_tool_results(messages: &mut [Message], budget: u64, protect_last: usize) -> usize {
    let mut total: u64 = messages.iter().map(estimate_message_tokens).sum();
    if total <= budget {
        return 0;
    }
    let tool_positions: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Tool)
        .map(|(i, _)| i)
        .collect();
    let prunable = tool_positions.len().saturating_sub(protect_last);

    let mut pruned = 0;
    for &index in &tool_positions[..prunable] {
        for block in &mut messages[index].content {
            if total <= budget {
                return pruned;
            }
            let ContentBlock::ToolResult { content, .. } = block else {
                continue;
            };
            let cost: u64 = content
                .iter()
                .map(|b| {
                    estimate_message_tokens(&Message::with_blocks(Role::Tool, vec![b.clone()]))
                })
                .sum();
            if cost < MIN_PRUNABLE_TOKENS {
                continue;
            }
            let replacement = vec![ContentBlock::text(stub(cost))];
            let new_cost =
                estimate_message_tokens(&Message::with_blocks(Role::Tool, replacement.clone()));
            *content = replacement;
            total = total.saturating_sub(cost) + new_cost;
            pruned += 1;
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::RequestedToolCall;
    use serde_json::json;

    fn round(id: &str, output_chars: usize) -> Vec<Message> {
        vec![
            Message::assistant_tool_calls(vec![RequestedToolCall {
                id: id.into(),
                name: "read_file".into(),
                arguments: json!({}),
            }]),
            Message::tool_result(id, "x".repeat(output_chars)),
        ]
    }

    fn result_text(m: &Message) -> String {
        match &m.content[0] {
            ContentBlock::ToolResult { content, .. } => {
                Message::with_blocks(Role::Tool, content.clone()).text()
            }
            _ => panic!("not a tool result"),
        }
    }

    #[test]
    fn nothing_happens_within_budget() {
        let mut messages = round("a", 4_000);
        assert_eq!(prune_tool_results(&mut messages, 10_000, 0), 0);
        assert_eq!(result_text(&messages[1]).len(), 4_000);
    }

    #[test]
    fn prunes_oldest_first_just_until_it_fits_and_keeps_every_message() {
        let mut messages = vec![Message::new(Role::User, "q")];
        messages.extend(round("a", 8_000)); // ~2000 tokens each
        messages.extend(round("b", 8_000));
        messages.extend(round("c", 8_000));
        let before = messages.len();

        let pruned = prune_tool_results(&mut messages, 4_500, 0);
        assert_eq!(pruned, 1, "one stub brings ~6000 down under 4500");
        assert_eq!(messages.len(), before);
        assert!(result_text(&messages[2]).starts_with("[tool output omitted"));
        assert_eq!(result_text(&messages[4]).len(), 8_000);
        assert_eq!(result_text(&messages[6]).len(), 8_000);
    }

    #[test]
    fn the_most_recent_results_are_protected() {
        let mut messages = round("a", 8_000);
        messages.extend(round("b", 8_000));
        prune_tool_results(&mut messages, 0, 1);
        assert!(result_text(&messages[1]).starts_with("[tool output omitted"));
        assert_eq!(result_text(&messages[3]).len(), 8_000);
    }

    #[test]
    fn small_results_are_left_alone() {
        let mut messages = round("a", 100);
        assert_eq!(prune_tool_results(&mut messages, 0, 0), 0);
    }
}
