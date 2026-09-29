use arbe_core::{ContextBreakdown, Message, Role};

use crate::breakdown::{count_stubbed_results, count_turns, measure_messages};
use crate::history::HistoryEntry;
use crate::prune::prune_tool_results;
use crate::tokens::{estimate_message_tokens, estimate_tokens};
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
    /// How much of `system_instructions` is instruction files substituted
    /// into the prompt template (estimator units), so the breakdown can
    /// report them apart from the harness's own prompt.
    pub instruction_file_tokens: u64,
}

impl ContextPipeline {
    fn summary_text(&self) -> Option<String> {
        self.conversation_summary.as_ref().map(|s| {
            format!("Summary of the earlier conversation (older messages were compacted to save space):\n{s}")
        })
    }

    /// The preamble's share of the breakdown: everything before history.
    fn measure_preamble(&self) -> ContextBreakdown {
        let sum = |texts: &[String]| texts.iter().map(|t| estimate_tokens(t)).sum::<u64>();
        let system = sum(&self.system_instructions);
        let instruction_files = self.instruction_file_tokens.min(system);
        ContextBreakdown {
            system_prompt: system - instruction_files,
            instructions: instruction_files + sum(&self.global_instructions),
            skills: sum(&self.skill_instructions),
            memory: sum(&self.memory_notes),
            summary: self.summary_text().map_or(0, |t| estimate_tokens(&t)),
            ..Default::default()
        }
    }

    fn preamble(&self) -> Vec<Message> {
        let summary = self.summary_text();
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

        let history_messages = &history_output.messages;
        let history_turns = count_turns(history_messages);
        let mut all_turns: Vec<u64> = history.iter().map(|e| e.turn_index).collect();
        all_turns.dedup();
        let breakdown = ContextBreakdown {
            history: measure_messages(history_messages),
            current_turn: measure_messages(std::slice::from_ref(&user_message)),
            history_turns,
            omitted_turns: (all_turns.len() as u64).saturating_sub(history_turns),
            stubbed_tool_results: count_stubbed_results(history_messages),
            ..self.measure_preamble()
        };

        let preamble_messages = preamble.len();
        let mut messages = preamble;
        messages.extend(history_output.messages);
        messages.push(user_message);

        let estimated_tokens = messages.iter().map(estimate_message_tokens).sum();

        ContextOutput {
            messages,
            estimated_tokens,
            truncated: history_output.truncated,
            pruned_tool_results,
            breakdown,
            preamble_messages,
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
            instruction_file_tokens: 0,
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
    fn the_breakdown_accounts_for_every_message_by_source() {
        let pipeline = ContextPipeline {
            system_instructions: vec!["x".repeat(400)],
            instruction_file_tokens: 60,
            skill_instructions: vec!["s".repeat(40)],
            memory_notes: vec!["m".repeat(20)],
            conversation_summary: Some("earlier".to_string()),
            ..Default::default()
        };
        let history: Vec<HistoryEntry> = (0..6)
            .flat_map(|t| {
                [
                    history_entry(t, &"q".repeat(400)),
                    HistoryEntry {
                        turn_index: t,
                        message: Message::new(Role::Assistant, "a".repeat(400)),
                    },
                ]
            })
            .collect();
        let out = pipeline.assemble(
            &TruncationStrategy,
            &history,
            &[],
            Message::new(Role::User, "now"),
            600,
        );
        let b = &out.breakdown;
        assert_eq!(b.system_prompt, 40);
        assert_eq!(b.instructions, 60);
        assert_eq!(b.skills, 10);
        assert_eq!(b.memory, 5);
        assert!(b.summary > 0);
        assert_eq!(b.tools, 0);
        assert_eq!(b.current_turn.user, 1);
        assert!(b.history_turns < 6 && b.history_turns > 0);
        assert_eq!(b.history_turns + b.omitted_turns, 6);
        assert_eq!(b.history.user, b.history_turns * 100);
        assert_eq!(b.total(), out.estimated_tokens);
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
