use arbe_core::{Message, Role};

use crate::history::HistoryEntry;
use crate::tokens::estimate_tokens;
use crate::{ContextInput, ContextOutput, ContextStrategy};

/// Assembles a full turn's context in the order from overall design §5.2:
/// base system instructions, global instructions, active skills, selected
/// session history, memory notes, then the user's turn. History selection
/// is delegated to whichever `ContextStrategy` the active profile
/// configures (truncation vs compact-with-summary), so swapping strategies
/// only changes step 4, not this ordering.
#[derive(Debug, Clone, Default)]
pub struct ContextPipeline {
    pub system_instructions: Vec<String>,
    pub global_instructions: Vec<String>,
    pub skill_instructions: Vec<String>,
    pub memory_notes: Vec<String>,
}

impl ContextPipeline {
    fn preamble(&self) -> Vec<Message> {
        self.system_instructions
            .iter()
            .chain(self.global_instructions.iter())
            .chain(self.skill_instructions.iter())
            .map(|text| Message::new(Role::System, text.clone()))
            .collect()
    }

    fn memory_messages(&self) -> Vec<Message> {
        self.memory_notes
            .iter()
            .map(|text| Message::new(Role::System, text.clone()))
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
        let memory_messages = self.memory_messages();

        let fixed_tokens: u64 = preamble
            .iter()
            .chain(memory_messages.iter())
            .map(|m| estimate_tokens(&m.content))
            .sum::<u64>()
            + estimate_tokens(&user_message.content);

        let history_budget = budget_tokens.saturating_sub(fixed_tokens);
        let history_output = strategy.build_context(ContextInput {
            session_history: history,
            budget_tokens: history_budget,
            pinned_turn_indices,
        });

        let mut messages = preamble;
        messages.extend(history_output.messages);
        messages.extend(memory_messages);
        messages.push(user_message);

        let estimated_tokens = messages.iter().map(|m| estimate_tokens(&m.content)).sum();

        ContextOutput {
            messages,
            estimated_tokens,
            truncated: history_output.truncated,
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
    fn orders_preamble_then_history_then_memory_then_user() {
        let pipeline = ContextPipeline {
            system_instructions: vec!["be helpful".to_string()],
            global_instructions: vec!["be terse".to_string()],
            skill_instructions: vec!["skill: rust".to_string()],
            memory_notes: vec!["remembered fact".to_string()],
        };
        let strategy = TruncationStrategy;
        let out = pipeline.assemble(
            &strategy,
            &[history_entry(0, "earlier turn")],
            &[],
            Message::new(Role::User, "current question"),
            10_000,
        );

        let contents: Vec<&str> = out.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec![
                "be helpful",
                "be terse",
                "skill: rust",
                "earlier turn",
                "remembered fact",
                "current question",
            ]
        );
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
        let history_contents: Vec<&str> = out
            .messages
            .iter()
            .filter(|m| m.role == Role::User && m.content != "q")
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(history_contents, vec!["bbbb"]);
        assert!(out.truncated);
    }
}
