use std::collections::HashSet;

use crate::history::HistoryEntry;
use crate::tokens::estimate_message_tokens;
use crate::{ContextInput, ContextOutput, ContextStrategy};

/// Decides, index by index, which history entries survive the budget:
/// pinned turns are always kept, then the most recent unpinned entries are
/// added (newest first) until the budget runs out. Returns per-index
/// `keep` flags (in original chronological order) plus the total estimated
/// token cost of everything that got dropped.
///
/// Shared by [`TruncationStrategy`] (drop silently) and
/// [`crate::CompactWithSummaryStrategy`] (drop + insert a summary), since
/// both need the identical selection rule to stay deterministic.
pub(crate) fn select_kept(
    history: &[HistoryEntry],
    budget_tokens: u64,
    pinned_turn_indices: &[u64],
) -> (Vec<bool>, u64) {
    let pinned_set: HashSet<u64> = pinned_turn_indices.iter().copied().collect();
    let costs: Vec<u64> = history
        .iter()
        .map(|e| estimate_message_tokens(&e.message))
        .collect();

    let mut keep = vec![false; history.len()];
    let mut budget = budget_tokens;
    for (i, entry) in history.iter().enumerate() {
        if pinned_set.contains(&entry.turn_index) {
            keep[i] = true;
            budget = budget.saturating_sub(costs[i]);
        }
    }

    for i in (0..history.len()).rev() {
        if keep[i] {
            continue;
        }
        if costs[i] <= budget {
            keep[i] = true;
            budget -= costs[i];
        }
    }

    let dropped_tokens = costs
        .iter()
        .enumerate()
        .filter(|(i, _)| !keep[*i])
        .map(|(_, c)| c)
        .sum();

    (keep, dropped_tokens)
}

/// Drops the oldest unpinned messages once the budget is exceeded, keeping
/// the most recent ones plus anything pinned. No summary is inserted for
/// what was dropped (see `CompactWithSummaryStrategy` for that).
#[derive(Debug, Clone, Copy, Default)]
pub struct TruncationStrategy;

impl ContextStrategy for TruncationStrategy {
    fn name(&self) -> &'static str {
        "truncation"
    }

    fn build_context(&self, input: ContextInput<'_>) -> ContextOutput {
        let (keep, dropped_tokens) = select_kept(
            input.session_history,
            input.budget_tokens,
            input.pinned_turn_indices,
        );

        let messages: Vec<arbe_core::Message> = input
            .session_history
            .iter()
            .zip(keep)
            .filter(|(_, k)| *k)
            .map(|(e, _)| e.message.clone())
            .collect();

        let estimated_tokens = messages.iter().map(estimate_message_tokens).sum();

        ContextOutput {
            messages,
            estimated_tokens,
            truncated: dropped_tokens > 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{Message, Role};

    fn entry(turn_index: u64, content: &str) -> HistoryEntry {
        HistoryEntry {
            turn_index,
            message: Message::new(Role::User, content),
        }
    }

    #[test]
    fn keeps_everything_within_budget() {
        let strategy = TruncationStrategy;
        let input = ContextInput {
            session_history: &[entry(0, "hi"), entry(1, "there")],
            budget_tokens: 1000,
            pinned_turn_indices: &[],
        };
        let out = strategy.build_context(input);
        assert_eq!(out.messages.len(), 2);
        assert!(!out.truncated);
    }

    #[test]
    fn drops_oldest_unpinned_first() {
        let strategy = TruncationStrategy;
        // Each message is ~1 token ("hi"/"there"-ish); budget of 1 forces
        // dropping all but the single most recent entry.
        let input = ContextInput {
            session_history: &[entry(0, "aaaa"), entry(1, "bbbb"), entry(2, "cccc")],
            budget_tokens: 1,
            pinned_turn_indices: &[],
        };
        let out = strategy.build_context(input);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].text(), "cccc");
        assert!(out.truncated);
    }

    #[test]
    fn pinned_turns_survive_even_when_old() {
        let strategy = TruncationStrategy;
        let input = ContextInput {
            session_history: &[entry(0, "aaaa"), entry(1, "bbbb"), entry(2, "cccc")],
            budget_tokens: 1,
            pinned_turn_indices: &[0],
        };
        let out = strategy.build_context(input);
        // Pinned turn 0 always kept; budget of 1 is fully consumed by it
        // (cost 1), so no unpinned entries fit.
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].text(), "aaaa");
        assert!(out.truncated);
    }

    #[test]
    fn preserves_chronological_order_of_kept_messages() {
        let strategy = TruncationStrategy;
        let input = ContextInput {
            session_history: &[entry(0, "aaaa"), entry(1, "bbbb"), entry(2, "cccc")],
            budget_tokens: 2,
            pinned_turn_indices: &[],
        };
        let out = strategy.build_context(input);
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.messages[0].text(), "bbbb");
        assert_eq!(out.messages[1].text(), "cccc");
    }

    #[test]
    fn select_kept_with_zero_budget_and_no_pinned_keeps_nothing() {
        let history = [entry(0, "aaaa"), entry(1, "bbbb")];
        let (keep, dropped_tokens) = select_kept(&history, 0, &[]);
        assert_eq!(keep, vec![false, false]);
        assert!(dropped_tokens > 0);
    }

    #[test]
    fn select_kept_keeps_all_pinned_turns_even_when_they_alone_exceed_the_budget() {
        let history = [entry(0, "aaaa"), entry(1, "bbbb"), entry(2, "cccc")];
        // Budget of 1 is smaller than the combined cost of the two pinned
        // entries — both must still be kept; saturating_sub must not panic.
        let (keep, _dropped_tokens) = select_kept(&history, 1, &[0, 1]);
        assert_eq!(keep, vec![true, true, false]);
    }
}
