//! What a model request's context is made of, in tokens (v2 plan P5.7).
//! The numbers are estimates (see `arbe_memory::estimate_tokens`),
//! calibrated against the provider's reported counts before they're
//! published, so they add up to roughly what the provider will bill.

use serde::{Deserialize, Serialize};

/// Tokens in a run of conversation messages, by kind of content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageTokens {
    /// Text the user wrote.
    pub user: u64,
    /// Text the model wrote.
    pub assistant: u64,
    /// The model's thinking, where it is sent back.
    pub thinking: u64,
    /// Tool calls: names and arguments.
    pub tool_calls: u64,
    /// Tool results.
    pub tool_results: u64,
    /// Images, at a flat estimate each.
    pub images: u64,
    /// Notes the harness adds, e.g. "[3 earlier messages omitted]".
    pub notices: u64,
}

impl MessageTokens {
    pub fn total(&self) -> u64 {
        self.user
            + self.assistant
            + self.thinking
            + self.tool_calls
            + self.tool_results
            + self.images
            + self.notices
    }

    fn scaled(self, factor: f64) -> Self {
        Self {
            user: scale(self.user, factor),
            assistant: scale(self.assistant, factor),
            thinking: scale(self.thinking, factor),
            tool_calls: scale(self.tool_calls, factor),
            tool_results: scale(self.tool_results, factor),
            images: scale(self.images, factor),
            notices: scale(self.notices, factor),
        }
    }
}

/// One model request's context, by source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBreakdown {
    /// The harness's own prompt (the profile's template), without the
    /// instruction files substituted into it.
    pub system_prompt: u64,
    /// Instruction files: `~/.arbe/instructions/agent.md` and the
    /// project's `agent.md`/`CLAUDE.md`.
    pub instructions: u64,
    /// Skill instructions (or the skill index, when skills load on demand).
    pub skills: u64,
    /// Memory notes (`memory.md` files).
    pub memory: u64,
    /// The summary standing in for compacted turns.
    pub summary: u64,
    /// Tool definitions offered to the model (names, descriptions, schemas).
    pub tools: u64,
    /// Earlier turns still in the context.
    pub history: MessageTokens,
    /// This turn so far: the user's message and every round since.
    pub current_turn: MessageTokens,
    /// Earlier turns included in full or condensed.
    pub history_turns: u64,
    /// Earlier turns left out to fit the budget (not counting compacted
    /// ones, which the summary stands in for).
    pub omitted_turns: u64,
    /// Tool results replaced by a short stub to fit the budget.
    pub stubbed_tool_results: u64,
}

impl ContextBreakdown {
    /// Everything that goes into the request.
    pub fn total(&self) -> u64 {
        self.system_prompt
            + self.instructions
            + self.skills
            + self.memory
            + self.summary
            + self.tools
            + self.history.total()
            + self.current_turn.total()
    }

    /// Every token figure multiplied by `factor` (the estimator's
    /// calibration); counts of turns and results are left alone.
    pub fn scaled(&self, factor: f64) -> Self {
        Self {
            system_prompt: scale(self.system_prompt, factor),
            instructions: scale(self.instructions, factor),
            skills: scale(self.skills, factor),
            memory: scale(self.memory, factor),
            summary: scale(self.summary, factor),
            tools: scale(self.tools, factor),
            history: self.history.scaled(factor),
            current_turn: self.current_turn.scaled(factor),
            ..self.clone()
        }
    }
}

/// A request's context against the limits it has to fit in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextUsage {
    pub breakdown: ContextBreakdown,
    /// `breakdown.total()`, for convenience.
    pub total_tokens: u64,
    /// What the harness lets the context grow to: the model's window
    /// minus room for the reply, or `ARBE_CONTEXT_BUDGET`.
    pub budget_tokens: u64,
    /// The model's context window.
    pub context_window: u64,
    /// History share of the budget at which automatic compaction starts,
    /// if it's on (`memory_strategy = "compact_summary"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_threshold_tokens: Option<u64>,
}

impl ContextUsage {
    pub fn new(
        breakdown: ContextBreakdown,
        budget_tokens: u64,
        context_window: u64,
        compaction_threshold_tokens: Option<u64>,
    ) -> Self {
        Self {
            total_tokens: breakdown.total(),
            breakdown,
            budget_tokens,
            context_window,
            compaction_threshold_tokens,
        }
    }

    /// `total_tokens` as a percentage of the budget (0 when there's none).
    pub fn percent_of_budget(&self) -> f64 {
        if self.budget_tokens == 0 {
            0.0
        } else {
            self.total_tokens as f64 * 100.0 / self.budget_tokens as f64
        }
    }
}

fn scale(tokens: u64, factor: f64) -> u64 {
    (tokens as f64 * factor).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ContextBreakdown {
        ContextBreakdown {
            system_prompt: 100,
            instructions: 50,
            skills: 10,
            memory: 5,
            summary: 20,
            tools: 300,
            history: MessageTokens {
                user: 10,
                assistant: 20,
                thinking: 0,
                tool_calls: 30,
                tool_results: 400,
                images: 0,
                notices: 0,
            },
            current_turn: MessageTokens {
                user: 7,
                ..Default::default()
            },
            history_turns: 3,
            omitted_turns: 1,
            stubbed_tool_results: 2,
        }
    }

    #[test]
    fn the_total_adds_up_every_source() {
        assert_eq!(sample().total(), 100 + 50 + 10 + 5 + 20 + 300 + 460 + 7);
    }

    #[test]
    fn scaling_changes_tokens_but_not_counts() {
        let scaled = sample().scaled(2.0);
        assert_eq!(scaled.tools, 600);
        assert_eq!(scaled.history.tool_results, 800);
        assert_eq!(scaled.history_turns, 3);
        assert_eq!(scaled.omitted_turns, 1);
        assert_eq!(scaled.stubbed_tool_results, 2);
        assert_eq!(scaled.total(), sample().total() * 2);
    }

    #[test]
    fn usage_reports_its_share_of_the_budget() {
        let usage = ContextUsage::new(sample(), 1_904, 8_192, None);
        assert_eq!(usage.total_tokens, 952);
        assert!((usage.percent_of_budget() - 50.0).abs() < 1e-9);
        assert_eq!(ContextUsage::default().percent_of_budget(), 0.0);
    }
}
