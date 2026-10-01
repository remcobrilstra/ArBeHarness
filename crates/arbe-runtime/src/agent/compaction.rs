//! LLM compaction (v2 plan P5.2): when history grows past most of the
//! budget, the model summarizes the oldest turns, and the summary stands in
//! for them from then on. The turns themselves stay in `turns.jsonl`; the
//! summary is saved in `compactions.jsonl` so it's computed once, not every
//! turn.

use arbe_core::{
    Compaction, HarnessError, Message, ProviderError, RequestedToolCall, Role, RuntimeEvent,
};
use arbe_memory::{HistoryEntry, estimate_message_tokens};
use arbe_providers::{CancellationToken, ModelRequest, ResponseAccumulator, stream_with_retry};
use futures_util::StreamExt;

use super::Agent;
use super::tools::truncate_middle;

/// Compact once history passes this share of the room it has (the budget
/// minus what every request carries anyway — see [`history_room`])...
pub(super) const TRIGGER_RATIO: f64 = 0.8;
/// ...down to about this share, so it doesn't happen again next turn.
const TARGET_RATIO: f64 = 0.4;
/// How much of each tool result the summarizer sees.
const RESULT_CHARS_IN_TRANSCRIPT: usize = 2_000;
const SUMMARY_MAX_TOKENS: u64 = 2_000;

const INSTRUCTIONS: &str = "\
You are summarizing the earlier part of a conversation between a user and an AI agent, so that the agent can continue the work without the full history.

Write a concise summary that preserves:
- the user's goals, requests and preferences;
- decisions made and why;
- important facts learned (names, values, error messages), each with where it is: file path, symbol, line number when known;
- what was done: each file changed (its path, and the functions or sections touched), commands run and their outcomes;
- anything still pending or promised, with the files it concerns.

Keep paths, symbols, commands and error text exactly as written; never replace a path with a description.

Omit pleasantries, dead ends and anything superseded. Use short bullet points. Do not address the user.";

/// Tokens every request spends before any history: the system prompt,
/// instruction files, skills, memory notes and tool definitions (the
/// summary isn't counted: compaction replaces it).
pub(super) fn fixed_tokens(breakdown: &arbe_core::ContextBreakdown) -> u64 {
    breakdown.system_prompt
        + breakdown.instructions
        + breakdown.skills
        + breakdown.memory
        + breakdown.tools
}

/// The part of `budget_tokens` history can use, given the latest request's
/// fixed costs (none known before the first request). With a small
/// context most of the budget is fixed cost, so measuring history against
/// the whole budget would never trigger compaction before trimming.
pub(super) fn history_room(budget_tokens: u64, last: Option<&arbe_core::ContextUsage>) -> u64 {
    let fixed = last.map_or(0, |usage| fixed_tokens(&usage.breakdown));
    budget_tokens.saturating_sub(fixed)
}

/// Which turns to compact: whole turns from the oldest, keeping the newest
/// turns that fit in the target share (always at least the newest one).
/// Returns the index of the last turn to compact, or `None` if nothing
/// should be (under the trigger, unless `force`, or only one turn).
pub(super) fn plan(history: &[HistoryEntry], budget: u64, force: bool) -> Option<u64> {
    let total: u64 = history
        .iter()
        .map(|e| estimate_message_tokens(&e.message))
        .sum();
    if !force && (total as f64) <= budget as f64 * TRIGGER_RATIO {
        return None;
    }
    let target = if force {
        0
    } else {
        (budget as f64 * TARGET_RATIO) as u64
    };

    let mut turns: Vec<(u64, u64)> = Vec::new(); // (turn index, cost), oldest first
    for entry in history {
        let cost = estimate_message_tokens(&entry.message);
        match turns.last_mut() {
            Some((index, sum)) if *index == entry.turn_index => *sum += cost,
            _ => turns.push((entry.turn_index, cost)),
        }
    }
    let mut kept_cost = 0;
    let mut first_kept = turns.len();
    for (i, (_, cost)) in turns.iter().enumerate().rev() {
        let is_newest = i + 1 == turns.len();
        if is_newest || kept_cost + cost <= target {
            kept_cost += cost;
            first_kept = i;
        } else {
            break;
        }
    }
    (first_kept > 0).then(|| turns[first_kept - 1].0)
}

/// The turns as plain text for the summarizer: who said what, which tools
/// were called with what, and the start of each result. `subject` names
/// what a call acts on (see `ToolExecutor::subject`): shown in full, since
/// clipping long arguments can cut out the path or command.
pub(super) fn render_transcript(
    entries: &[HistoryEntry],
    subject: impl Fn(&RequestedToolCall) -> Option<String>,
) -> String {
    let mut out = String::new();
    for entry in entries {
        let message = &entry.message;
        match message.role {
            Role::User => out.push_str(&format!("User: {}\n", message.text())),
            Role::Assistant => {
                let text = message.text();
                if !text.is_empty() {
                    out.push_str(&format!("Assistant: {text}\n"));
                }
                for call in message.tool_uses() {
                    let args = truncate_middle(call.arguments.to_string(), 300);
                    match subject(&call) {
                        Some(on) => {
                            out.push_str(&format!("[called {} on {on} ({args})]\n", call.name))
                        }
                        None => out.push_str(&format!("[called {}({args})]\n", call.name)),
                    }
                }
            }
            Role::Tool => {
                for block in &message.content {
                    if let arbe_core::ContentBlock::ToolResult {
                        content, is_error, ..
                    } = block
                    {
                        let text = Message::with_blocks(Role::Tool, content.clone()).text();
                        let label = if *is_error { "error" } else { "result" };
                        out.push_str(&format!(
                            "[{label}: {}]\n",
                            truncate_middle(text, RESULT_CHARS_IN_TRANSCRIPT)
                        ));
                    }
                }
            }
            Role::System => {}
        }
    }
    out
}

/// Summarizes history and makes the summary the session's context from now
/// on. `force` compacts everything but the newest turn (the `/compact`
/// command); otherwise only when history is over the trigger. Returns the
/// new compaction, or `None` if there was nothing to do.
pub(super) async fn compact(
    agent: &Agent,
    force: bool,
    cancel: &CancellationToken,
) -> Result<Option<Compaction>, HarnessError> {
    let (through, entries, previous) = {
        let state = agent.state();
        let budget = state.calibration.budget_in_estimate_units(history_room(
            agent.settings.budget_tokens,
            state.last_context.as_ref(),
        ));
        let Some(through) = plan(&state.history, budget, force) else {
            return Ok(None);
        };
        let entries: Vec<HistoryEntry> = state
            .history
            .iter()
            .filter(|e| e.turn_index <= through)
            .cloned()
            .collect();
        (through, entries, state.summary.clone())
    };

    let mut prompt = String::new();
    if let Some(previous) = &previous {
        prompt.push_str(&format!(
            "Summary of the conversation before this part:\n{}\n\n",
            previous.summary
        ));
    }
    // The transcript must itself fit: clip it to most of the budget.
    let max_chars = (agent.settings.budget_tokens as usize).saturating_mul(3);
    prompt.push_str("Conversation to summarize:\n");
    let registry = agent.registry_snapshot();
    let subject = |call: &RequestedToolCall| {
        registry
            .get(&call.name)
            .ok()
            .and_then(|tool| tool.subject(&call.arguments))
    };
    prompt.push_str(&truncate_middle(
        render_transcript(&entries, subject),
        max_chars,
    ));

    let request = ModelRequest {
        model: agent.settings.model.clone(),
        messages: vec![
            Message::new(Role::System, INSTRUCTIONS),
            Message::new(Role::User, prompt),
        ],
        temperature: agent.settings.temperature,
        max_tokens: SUMMARY_MAX_TOKENS,
        tools: Vec::new(),
        thinking_budget_tokens: None,
    };
    let mut stream = stream_with_retry(
        agent.provider.as_ref(),
        request,
        cancel,
        &agent.settings.retry,
        |_| {},
    )
    .await
    .map_err(summarize_error)?;
    let mut acc = ResponseAccumulator::new();
    while let Some(event) = stream.next().await {
        acc.push(event.map_err(summarize_error)?);
    }
    let response = acc.finish();
    let summary = response.message.text().trim().to_string();
    if summary.is_empty() {
        return Err(HarnessError::Provider(ProviderError::Internal(
            "the model returned an empty summary".into(),
        )));
    }

    let compaction = Compaction {
        through_turn_index: through,
        summary,
        usage: response.usage,
        created_at: chrono::Utc::now(),
    };
    agent
        .store
        .append_compaction(agent.session_id(), &compaction)
        .map_err(|e| {
            HarnessError::Memory(arbe_core::MemoryError::StoreUnavailable(e.to_string()))
        })?;

    let compacted_messages = entries.len() as u64;
    {
        let mut state = agent.state();
        state.history.retain(|e| e.turn_index > through);
        state.summary = Some(compaction.clone());
        super::add_usage(&mut state.meta, compaction.usage, agent.settings.pricing);
        if let Err(err) = agent.store.save_meta(&state.meta) {
            tracing::warn!(%err, "failed to update session metadata");
        }
    }
    agent.events.publish(RuntimeEvent::CompactionPerformed {
        session_id: agent.session_id(),
        turn_id: None,
        compacted_messages,
    });
    Ok(Some(compaction))
}

fn summarize_error(err: ProviderError) -> HarnessError {
    match err {
        ProviderError::Cancelled => HarnessError::Cancelled,
        other => HarnessError::Provider(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn turn(index: u64, chars: usize) -> Vec<HistoryEntry> {
        [
            Message::new(Role::User, format!("question {index}")),
            Message::new(Role::Assistant, "a".repeat(chars)),
        ]
        .into_iter()
        .map(|message| HistoryEntry {
            turn_index: index,
            message,
        })
        .collect()
    }

    #[test]
    fn nothing_is_compacted_under_the_trigger() {
        let history: Vec<_> = (0..3).flat_map(|i| turn(i, 400)).collect();
        assert_eq!(plan(&history, 10_000, false), None);
    }

    #[test]
    fn over_the_trigger_the_oldest_turns_go_down_to_the_target() {
        // Four turns of ~1000 tokens against a 4500 budget: 4000 > 3600.
        let history: Vec<_> = (0..4).flat_map(|i| turn(i, 4_000)).collect();
        // Target 1800: keep only the newest turn (the second-newest would
        // make it ~2000).
        assert_eq!(plan(&history, 4_500, false), Some(2));
    }

    #[test]
    fn history_is_measured_against_the_room_left_after_fixed_costs() {
        // A small context: 4096 budget, 3000 of it spent on the prompt,
        // tools and memory before any history.
        let usage = arbe_core::ContextUsage::new(
            arbe_core::ContextBreakdown {
                system_prompt: 1_000,
                instructions: 200,
                skills: 100,
                memory: 200,
                tools: 1_500,
                summary: 400,
                ..Default::default()
            },
            4_096,
            8_192,
            None,
        );
        assert_eq!(history_room(4_096, Some(&usage)), 1_096);
        assert_eq!(history_room(4_096, None), 4_096);
        assert_eq!(history_room(1_000, Some(&usage)), 0);
        // ~2000 tokens of history: under 80% of the whole budget, but far
        // over the room history actually has.
        let history: Vec<_> = (0..4).flat_map(|i| turn(i, 2_000)).collect();
        assert_eq!(plan(&history, 4_096, false), None);
        assert!(plan(&history, history_room(4_096, Some(&usage)), false).is_some());
    }

    #[test]
    fn force_keeps_just_the_newest_turn_and_one_turn_is_never_compacted() {
        let history: Vec<_> = (0..3).flat_map(|i| turn(i, 40)).collect();
        assert_eq!(plan(&history, 1_000_000, true), Some(1));
        assert_eq!(plan(&turn(0, 40), 1_000_000, true), None);
    }

    #[test]
    fn the_transcript_shows_speakers_tool_calls_and_clipped_results() {
        let call = RequestedToolCall {
            id: "c".into(),
            name: "read_file".into(),
            arguments: json!({"path": "src/main.rs"}),
        };
        let entries: Vec<HistoryEntry> = [
            Message::new(Role::User, "what's in main?"),
            Message::assistant_tool_calls(vec![call]),
            Message::tool_result("c", "x".repeat(10_000)),
            Message::new(Role::Assistant, "it starts the TUI"),
        ]
        .into_iter()
        .map(|message| HistoryEntry {
            turn_index: 0,
            message,
        })
        .collect();
        let text = render_transcript(&entries, |_| None);
        assert!(text.contains("User: what's in main?"));
        assert!(text.contains(r#"[called read_file({"path":"src/main.rs"})]"#));
        assert!(text.contains("characters omitted"));
        assert!(text.contains("Assistant: it starts the TUI"));
        assert!(text.len() < 3_000);
    }

    #[test]
    fn a_calls_subject_survives_clipped_arguments() {
        let call = RequestedToolCall {
            id: "c".into(),
            name: "edit_file".into(),
            arguments: json!({
                "find": "a".repeat(1_000),
                "path": "crates/deep/src/lib.rs",
                "replace": "b".repeat(1_000),
            }),
        };
        let entries = vec![HistoryEntry {
            turn_index: 0,
            message: Message::assistant_tool_calls(vec![call]),
        }];
        assert!(!render_transcript(&entries, |_| None).contains("crates/deep/src/lib.rs"));
        let text = render_transcript(&entries, |call| {
            call.arguments["path"].as_str().map(str::to_string)
        });
        assert!(
            text.contains("[called edit_file on crates/deep/src/lib.rs ("),
            "{text}"
        );
    }
}
