use std::collections::HashSet;

use arbe_core::{ContentBlock, Role};

use crate::history::HistoryEntry;
use crate::tokens::estimate_message_tokens;
use crate::{ContextInput, ContextOutput, ContextStrategy};

/// Decides which history entries survive the budget, **a whole turn at a
/// time**: a turn's tool calls and their results are never split (a lone
/// half of a pair is rejected by providers).
///
/// Pinned turns are always kept in full. Then, newest turn first, each
/// turn is kept in full if it fits; otherwise in *condensed* form — just
/// its opening user message and, if it's a plain answer, its final
/// assistant message — if that fits; otherwise selection stops, so what's
/// kept is always a contiguous recent stretch of the conversation (plus
/// pinned turns).
///
/// When something has to be dropped, the cut is then moved forward to a
/// turn index that's a multiple of [`CUT_QUANTUM`]: without that, every new
/// turn at the budget limit would push out exactly one old turn, the start
/// of the history would change on every request, and provider prompt
/// caching would never hit. With it, the history keeps starting at the
/// same turn for several turns in a row (at the cost of sometimes leaving
/// out a few more old turns than strictly necessary).
///
/// Returns per-index `keep` flags (chronological order) and the estimated
/// token cost of everything dropped. Shared by [`TruncationStrategy`] and
/// [`crate::CompactWithSummaryStrategy`] so both select identically.
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
    let turns = group_turns(history);

    let mut keep = vec![false; history.len()];
    let mut budget = budget_tokens;
    for turn in &turns {
        if pinned_set.contains(&history[turn.start].turn_index) {
            keep[turn.start..turn.end].fill(true);
            budget = budget.saturating_sub(turn.cost(&costs));
        }
    }

    for turn in turns.iter().rev() {
        if pinned_set.contains(&history[turn.start].turn_index) {
            continue;
        }
        let full_cost = turn.cost(&costs);
        if full_cost <= budget {
            keep[turn.start..turn.end].fill(true);
            budget -= full_cost;
            continue;
        }
        let condensed = condensed_entries(history, turn);
        let condensed_cost: u64 = condensed.iter().map(|&i| costs[i]).sum();
        if !condensed.is_empty() && condensed_cost <= budget {
            for i in condensed {
                keep[i] = true;
            }
        }
        // Whether condensed or dropped, this turn didn't fit whole: stop
        // here so older turns never appear without the ones after them.
        break;
    }

    quantize_cut(history, &mut keep, &pinned_set);

    let dropped_tokens = costs
        .iter()
        .zip(&keep)
        .filter(|(_, kept)| !**kept)
        .map(|(c, _)| c)
        .sum();

    (keep, dropped_tokens)
}

/// Cut points are rounded up to turn indices divisible by this.
pub const CUT_QUANTUM: u64 = 4;

/// Moves the start of the kept (unpinned) history forward to the next
/// multiple of [`CUT_QUANTUM`], if anything was dropped at all — but only
/// when at least `2 * CUT_QUANTUM` turns remain afterwards: caching only
/// pays off for long histories, and on a short one rounding up would throw
/// away most of what's left.
fn quantize_cut(history: &[HistoryEntry], keep: &mut [bool], pinned: &HashSet<u64>) {
    let unpinned = |i: &usize| !pinned.contains(&history[*i].turn_index);
    let dropped_any = (0..history.len()).filter(unpinned).any(|i| !keep[i]);
    let Some(oldest_kept) = (0..history.len())
        .filter(unpinned)
        .find(|&i| keep[i])
        .map(|i| history[i].turn_index)
    else {
        return;
    };
    if !dropped_any {
        return;
    }
    let cut = oldest_kept
        .div_ceil(CUT_QUANTUM)
        .saturating_mul(CUT_QUANTUM);
    let mut remaining: Vec<u64> = (0..history.len())
        .filter(unpinned)
        .map(|i| history[i].turn_index)
        .filter(|t| *t >= cut)
        .collect();
    remaining.dedup();
    if (remaining.len() as u64) < 2 * CUT_QUANTUM {
        return;
    }
    for i in 0..history.len() {
        if unpinned(&i) && history[i].turn_index < cut {
            keep[i] = false;
        }
    }
}

/// A run of consecutive entries sharing one `turn_index`: `start..end`.
struct TurnSpan {
    start: usize,
    end: usize,
}

impl TurnSpan {
    fn cost(&self, costs: &[u64]) -> u64 {
        costs[self.start..self.end].iter().sum()
    }
}

fn group_turns(history: &[HistoryEntry]) -> Vec<TurnSpan> {
    let mut turns: Vec<TurnSpan> = Vec::new();
    for (i, entry) in history.iter().enumerate() {
        match turns.last_mut() {
            Some(t) if history[t.start].turn_index == entry.turn_index => t.end = i + 1,
            _ => turns.push(TurnSpan {
                start: i,
                end: i + 1,
            }),
        }
    }
    turns
}

/// A turn reduced to what reads coherently on its own: the first user
/// message, plus the final assistant message when it's a plain answer
/// (no tool calls, which would need their results alongside).
fn condensed_entries(history: &[HistoryEntry], turn: &TurnSpan) -> Vec<usize> {
    let plain = |i: &usize| {
        !history[*i].message.content.iter().any(|b| {
            matches!(
                b,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            )
        })
    };
    let first_user = (turn.start..turn.end)
        .find(|&i| history[i].message.role == Role::User)
        .filter(plain);
    let final_answer = (turn.start..turn.end)
        .rev()
        .find(|&i| history[i].message.role == Role::Assistant)
        .filter(plain);
    first_user.into_iter().chain(final_answer).collect()
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
            pruned_tool_results: 0,
            ..Default::default()
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

    fn tool_turn(turn_index: u64, answer: &str, tool_output: &str) -> Vec<HistoryEntry> {
        let call = arbe_core::RequestedToolCall {
            id: format!("c{turn_index}"),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        [
            Message::new(Role::User, "question"),
            Message::assistant_tool_calls(vec![call]),
            Message::tool_result(format!("c{turn_index}"), tool_output),
            Message::new(Role::Assistant, answer),
        ]
        .into_iter()
        .map(|message| HistoryEntry {
            turn_index,
            message,
        })
        .collect()
    }

    #[test]
    fn a_turn_that_fits_is_kept_whole_with_its_tool_pairs() {
        let history = tool_turn(0, "done", "short");
        let (keep, dropped) = select_kept(&history, 1_000, &[]);
        assert_eq!(keep, vec![true; 4]);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn a_turn_too_big_to_keep_whole_is_condensed_to_question_and_answer() {
        let history = tool_turn(0, "done", &"x".repeat(4_000));
        let (keep, _) = select_kept(&history, 100, &[]);
        // user + final answer kept; the tool call and its huge result go
        // together, never one without the other.
        assert_eq!(keep, vec![true, false, false, true]);
    }

    #[test]
    fn selection_stops_at_the_first_turn_that_does_not_fit_whole() {
        let mut history = vec![entry(0, "tiny")];
        history.extend(tool_turn(1, "done", &"x".repeat(4_000)));
        history.extend(tool_turn(2, "ok", "short"));
        let (keep, _) = select_kept(&history, 100, &[]);
        // Turn 2 whole, turn 1 condensed — and turn 0 (which would fit)
        // is not kept, so history stays contiguous.
        assert_eq!(
            keep,
            vec![false, true, false, false, true, true, true, true, true]
        );
    }

    fn kept_turns(history: &[HistoryEntry], budget: u64) -> Vec<u64> {
        let (keep, _) = select_kept(history, budget, &[]);
        let mut turns: Vec<u64> = history
            .iter()
            .zip(keep)
            .filter(|(_, k)| *k)
            .map(|(e, _)| e.turn_index)
            .collect();
        turns.dedup();
        turns
    }

    #[test]
    fn the_cut_moves_in_quanta_so_the_history_prefix_stays_stable() {
        // Turns of 100 tokens each.
        let turns =
            |n: u64| -> Vec<HistoryEntry> { (0..n).map(|i| entry(i, &"x".repeat(400))).collect() };
        let first = |n, budget| kept_turns(&turns(n), budget)[0];
        // Everything fits: nothing is cut.
        assert_eq!(kept_turns(&turns(5), 10_000), vec![0, 1, 2, 3, 4]);
        // Room for 21 of 30 turns → minimal start 9 → rounded up to 12.
        assert_eq!(first(30, 2_150), 12);
        // Over the next turns the history keeps starting at turn 12 — same
        // prefix, so the provider's prompt cache keeps hitting...
        assert_eq!(first(31, 2_150), 12);
        assert_eq!(first(32, 2_150), 12);
        assert_eq!(first(33, 2_150), 12);
        // ...until the minimal start passes it, then it jumps a quantum.
        assert_eq!(first(34, 2_150), 16);
        // Short histories aren't rounded: room for 5 of 10 keeps 5..9.
        assert_eq!(kept_turns(&turns(10), 550), vec![5, 6, 7, 8, 9]);
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
