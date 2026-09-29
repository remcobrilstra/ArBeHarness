//! Text for the header's context figure and the `/context` breakdown.

use arbe_runtime::arbe_core::{ContextUsage, MessageTokens};

/// `12.3k`-style token counts, short enough for the header.
pub fn short_tokens(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=99_999 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{}k", tokens / 1_000),
    }
}

/// The header's context figure: `~12.3k/120k (10%)`, or just the
/// estimate before any breakdown has arrived.
pub fn header_label(usage: Option<&ContextUsage>, fallback_estimate: u64) -> String {
    match usage {
        Some(u) if u.budget_tokens > 0 => format!(
            "~{}/{} ({:.0}%)",
            short_tokens(u.total_tokens),
            short_tokens(u.budget_tokens),
            u.percent_of_budget()
        ),
        _ => format!("~{fallback_estimate}"),
    }
}

/// The `/context` report: every source with its tokens and share of the
/// total, then the limits.
pub fn report(usage: &ContextUsage) -> String {
    let b = &usage.breakdown;
    let total = usage.total_tokens.max(1);
    let mut lines = vec![format!(
        "Context of the latest model call: ~{} tokens ({:.0}% of the {} budget; window {})",
        usage.total_tokens,
        usage.percent_of_budget(),
        usage.budget_tokens,
        usage.context_window
    )];
    let mut row = |label: &str, tokens: u64| {
        if tokens > 0 {
            lines.push(format!(
                "  {label:<22} {tokens:>8}  {:>5.1}%",
                tokens as f64 * 100.0 / total as f64
            ));
        }
    };
    row("system prompt", b.system_prompt);
    row("instruction files", b.instructions);
    row("skills", b.skills);
    row("memory", b.memory);
    row("compaction summary", b.summary);
    row("tool definitions", b.tools);
    message_rows(&mut row, "history", &b.history);
    message_rows(&mut row, "this turn", &b.current_turn);

    lines.push(format!(
        "  earlier turns: {} in context, {} left out",
        b.history_turns, b.omitted_turns
    ));
    if b.stubbed_tool_results > 0 {
        lines.push(format!(
            "  {} old tool result(s) replaced by a stub to save room",
            b.stubbed_tool_results
        ));
    }
    if let Some(threshold) = usage.compaction_threshold_tokens {
        lines.push(format!(
            "  automatic compaction starts when history passes ~{threshold} tokens"
        ));
    }
    lines.join("\n")
}

fn message_rows(row: &mut impl FnMut(&str, u64), scope: &str, tokens: &MessageTokens) {
    for (kind, value) in [
        ("user", tokens.user),
        ("assistant", tokens.assistant),
        ("thinking", tokens.thinking),
        ("tool calls", tokens.tool_calls),
        ("tool results", tokens.tool_results),
        ("images", tokens.images),
        ("notes", tokens.notices),
    ] {
        row(&format!("{scope}: {kind}"), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_runtime::arbe_core::ContextBreakdown;

    fn usage() -> ContextUsage {
        ContextUsage::new(
            ContextBreakdown {
                system_prompt: 1_000,
                tools: 3_000,
                history: MessageTokens {
                    tool_results: 6_000,
                    ..Default::default()
                },
                history_turns: 4,
                omitted_turns: 2,
                stubbed_tool_results: 1,
                ..Default::default()
            },
            100_000,
            128_000,
            Some(80_000),
        )
    }

    #[test]
    fn the_header_shows_size_budget_and_share() {
        assert_eq!(header_label(Some(&usage()), 0), "~10.0k/100k (10%)");
        assert_eq!(header_label(None, 42), "~42");
        assert_eq!(short_tokens(999), "999");
    }

    #[test]
    fn the_report_lists_nonzero_sources_and_the_limits() {
        let text = report(&usage());
        assert!(text.contains("~10000 tokens (10% of the 100000 budget; window 128000)"));
        assert!(text.contains("tool definitions"));
        assert!(text.contains("history: tool results"));
        assert!(text.contains("60.0%"));
        assert!(!text.contains("skills"), "zero rows are left out");
        assert!(text.contains("4 in context, 2 left out"));
        assert!(text.contains("1 old tool result(s)"));
        assert!(text.contains("~80000 tokens"));
    }
}
