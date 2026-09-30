use arbe_core::{Message, Role};

use crate::tokens::estimate_message_tokens;
use crate::truncation::select_kept;
use crate::{ContextInput, ContextOutput, ContextStrategy};

/// Same selection rule as [`crate::TruncationStrategy`] (pinned turns kept,
/// most recent unpinned entries kept until budget runs out), but instead of
/// silently dropping older messages it prepends one system notice saying
/// how much was left out, so the model knows the conversation started
/// earlier.
///
/// It's the history strategy of `memory_strategy = "compact_summary"`,
/// whose actual summary is written by the model: the runtime's compaction
/// (`agent::compaction`) summarizes old turns before this strategy ever
/// has to drop any, so this only acts on what's still too big after that.
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncateWithNoticeStrategy;

impl ContextStrategy for TruncateWithNoticeStrategy {
    fn name(&self) -> &'static str {
        "compact_summary"
    }

    fn build_context(&self, input: ContextInput<'_>) -> ContextOutput {
        let (keep, dropped_tokens) = select_kept(
            input.session_history,
            input.budget_tokens,
            input.pinned_turn_indices,
        );
        let dropped_count = keep.iter().filter(|k| !**k).count();

        let mut messages: Vec<Message> = Vec::new();
        if dropped_count > 0 {
            messages.push(Message::new(
                Role::System,
                format!(
                    "[compacted: {dropped_count} earlier message(s) omitted, ~{dropped_tokens} tokens]"
                ),
            ));
        }
        messages.extend(
            input
                .session_history
                .iter()
                .zip(keep)
                .filter(|(_, k)| *k)
                .map(|(e, _)| e.message.clone()),
        );

        let estimated_tokens = messages.iter().map(estimate_message_tokens).sum();

        ContextOutput {
            messages,
            estimated_tokens,
            truncated: dropped_count > 0,
            pruned_tool_results: 0,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HistoryEntry;

    fn entry(turn_index: u64, content: &str) -> HistoryEntry {
        HistoryEntry {
            turn_index,
            message: Message::new(Role::User, content),
        }
    }

    #[test]
    fn no_summary_when_nothing_dropped() {
        let strategy = TruncateWithNoticeStrategy;
        let out = strategy.build_context(ContextInput {
            session_history: &[entry(0, "hi")],
            budget_tokens: 1000,
            pinned_turn_indices: &[],
        });
        assert_eq!(out.messages.len(), 1);
        assert!(!out.truncated);
    }

    #[test]
    fn prepends_a_summary_message_when_entries_are_dropped() {
        let strategy = TruncateWithNoticeStrategy;
        let out = strategy.build_context(ContextInput {
            session_history: &[entry(0, "aaaa"), entry(1, "bbbb"), entry(2, "cccc")],
            budget_tokens: 1,
            pinned_turn_indices: &[],
        });
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.messages[0].role, Role::System);
        assert!(out.messages[0].text().contains("2 earlier message"));
        assert_eq!(out.messages[1].text(), "cccc");
        assert!(out.truncated);
    }
}
