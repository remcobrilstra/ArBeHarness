use arbe_core::{Message, Role};

use crate::history::HistoryEntry;
use crate::prune::prune_tool_results;
use crate::tokens::estimate_message_tokens;
use crate::{ContextInput, ContextOutput, ContextStrategy};

/// Assembles a full turn's context: system instructions, global
/// instructions, active skills, memory notes, the conversation summary (if
/// older turns were compacted), the selected history, then the user's
/// turn. History selection is delegated to whichever `ContextStrategy` the
/// profile configures, so swapping strategies only changes that step.
///
/// Everything that rarely changes comes first, in a fixed order: providers
/// cache a request's longest unchanged *prefix*, so stable content ahead of
/// the growing history is what gets cache hits turn after turn. (Overall
/// design §5.2 put memory notes after the history; they were moved into
/// the stable prefix for this reason.)
#[derive(Debug, Clone, Default)]
pub struct ContextPipeline {
    pub system_instructions: Vec<String>,
    pub global_instructions: Vec<String>,
    pub skill_instructions: Vec<String>,
    pub memory_notes: Vec<String>,
    /// A summary standing in for compacted older turns; placed right after
    /// the instructions, before the remaining history.
    pub conversation_summary: Option<String>,
}

impl ContextPipeline {
    fn preamble(&self) -> Vec<Message> {
        let summary = self.conversation_summary.as_ref().map(|s| {
            format!("Summary of the earlier conversation (older messages were compacted to save space):\n{s}")
        });
        self.system_instructions
            .iter()
            .chain(self.global_instructions.iter())
            .chain(self.skill_instructions.iter())
            .chain(self.memory_notes.iter())
            .cloned()
            .chain(summary)
            .map(|text| Message::new(Role::System, text))
            .collect()
    }

    /// `budget_tokens` is the total budget for the assembled context,
    /// including the fixed preamble/memory/user cost — only what's left
    /// over after those is handed to the history strategy, so a large
    /// instruction set or memory file deterministically leaves less room
    /// for history rather than silently blowing the overall budget.
    pub fn assemble(
        &self,
        strategy: &dyn ContextStrategy,
        history: &[HistoryEntry],
        pinned_turn_indices: &[u64],
        user_message: Message,
        budget_tokens: u64,
    ) -> ContextOutput {
        let preamble = self.preamble();

        let fixed_tokens: u64 = preamble.iter().map(estimate_message_tokens).sum::<u64>()
            + estimate_message_tokens(&user_message);

        let history_budget = budget_tokens.saturating_sub(fixed_tokens);

        // Over budget: stub out old tool results first (cheap, keeps every
        // message), and only let the strategy drop turns if that isn't
        // enough. The copy is only made when pruning is needed.
        let history_cost: u64 = history
            .iter()
            .map(|e| estimate_message_tokens(&e.message))
            .sum();
        let mut pruned_tool_results = 0;
        let pruned_history: Option<Vec<HistoryEntry>> =
            (history_cost > history_budget).then(|| {
                let mut messages: Vec<Message> =
                    history.iter().map(|e| e.message.clone()).collect();
                pruned_tool_results = prune_tool_results(&mut messages, history_budget, 0);
                history
                    .iter()
                    .zip(messages)
                    .map(|(e, message)| HistoryEntry {
                        turn_index: e.turn_index,
                        message,
                    })
                    .collect()
            });
        let history_output = strategy.build_context(ContextInput {
            session_history: pruned_history.as_deref().unwrap_or(history),
            budget_tokens: history_budget,
            pinned_turn_indices,
        });

        let mut messages = preamble;
        messages.extend(history_output.messages);
        messages.push(user_message);

        let estimated_tokens = messages.iter().map(estimate_message_tokens).sum();

        ContextOutput {
            messages,
            estimated_tokens,
            truncated: history_output.truncated,
            pruned_tool_results,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TruncationStrategy;

    fn history_entry(turn_index: u64, content: &str) -> HistoryEntry {
        HistoryEntry {
            turn_index,
            message: Message::new(Role::User, content),
        }
    }

    #[test]
    fn orders_stable_content_first_then_history_then_user() {
        let pipeline = ContextPipeline {
            system_instructions: vec!["be helpful".to_string()],
            global_instructions: vec!["be terse".to_string()],
            skill_instructions: vec!["skill: rust".to_string()],
            memory_notes: vec!["remembered fact".to_string()],
            conversation_summary: None,
        };
        let strategy = TruncationStrategy;
        let out = pipeline.assemble(
            &strategy,
            &[history_entry(0, "earlier turn")],
            &[],
            Message::new(Role::User, "current question"),
            10_000,
        );

        let contents: Vec<String> = out.messages.iter().map(|m| m.text()).collect();
        assert_eq!(
            contents,
            vec![
                "be helpful",
                "be terse",
                "skill: rust",
                "remembered fact",
                "earlier turn",
                "current question",
            ]
        );
    }

    #[test]
    fn a_conversation_summary_sits_between_the_instructions_and_the_history() {
        let pipeline = ContextPipeline {
            system_instructions: vec!["be helpful".to_string()],
            conversation_summary: Some("- fixed the login bug".to_string()),
            ..Default::default()
        };
        let out = pipeline.assemble(
            &TruncationStrategy,
            &[history_entry(5, "recent turn")],
            &[],
            Message::new(Role::User, "next"),
            10_000,
        );
        let texts: Vec<String> = out.messages.iter().map(|m| m.text()).collect();
        assert_eq!(texts[0], "be helpful");
        assert_eq!(out.messages[1].role, Role::System);
        assert!(texts[1].ends_with("- fixed the login bug"));
        assert_eq!(texts[2], "recent turn");
        assert_eq!(texts[3], "next");
    }

    #[test]
    fn old_tool_output_is_pruned_before_any_turn_is_dropped() {
        let call = arbe_core::RequestedToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({}),
        };
        let history: Vec<HistoryEntry> = [
            Message::new(Role::User, "read it"),
            Message::assistant_tool_calls(vec![call]),
            Message::tool_result("c", "x".repeat(40_000)),
            Message::new(Role::Assistant, "it says x"),
        ]
        .into_iter()
        .map(|message| HistoryEntry {
            turn_index: 0,
            message,
        })
        .collect();
        let out = ContextPipeline::default().assemble(
            &TruncationStrategy,
            &history,
            &[],
            Message::new(Role::User, "and?"),
            1_000,
        );
        // The whole turn survived, with its tool output stubbed.
        assert_eq!(out.messages.len(), 5);
        assert_eq!(out.pruned_tool_results, 1);
        assert!(!out.truncated);
        assert!(out.estimated_tokens < 1_000);
    }

    #[test]
    fn fixed_costs_shrink_the_budget_left_for_history() {
        let pipeline = ContextPipeline {
            system_instructions: vec!["x".repeat(40)], // ~10 tokens
            ..Default::default()
        };
        let strategy = TruncationStrategy;
        let out = pipeline.assemble(
            &strategy,
            &[history_entry(0, "aaaa"), history_entry(1, "bbbb")],
            &[],
            Message::new(Role::User, "q"),
            12, // only ~1 token left for history after the preamble + user
        );
        // Only the most recent history entry should fit.
        let history_contents: Vec<String> = out
            .messages
            .iter()
            .filter(|m| m.role == Role::User && m.text() != "q")
            .map(|m| m.text())
            .collect();
        assert_eq!(history_contents, vec!["bbbb"]);
        assert!(out.truncated);
    }
}
