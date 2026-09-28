//! One round of tool calls: hooks and approval for each call in order,
//! then execution — concurrently where tools allow it — and the results
//! assembled into one tool-result message, in the model's original order.

use std::sync::Arc;

use arbe_core::{
    ApprovalDecision, ContentBlock, Message, RequestedToolCall, Role, RuntimeEvent, ToolCallId,
    ToolError, ToolInvocation, ToolResult, TurnId,
};
use arbe_hooks::HookPhase;
use arbe_providers::CancellationToken;
use arbe_tools::{Authorization, Authorized, ToolContext, authorize};
use futures_util::future::join_all;
use serde_json::{Value, json};

use super::Agent;
use super::hooks::{self, ToolCallPayload, ToolCallVerdict, ToolResultPayload};

/// What one call in the round turned into, before execution.
enum Slot {
    /// Already has its result (denied, vetoed, invalid).
    Done { text: String, is_error: bool },
    /// Approved; runs in the execution phase.
    Approved {
        authorized: Authorized,
        id: ToolCallId,
    },
}

/// Keeps a tool result to at most `max_chars`, preserving the head and
/// tail (where errors and summaries usually are) around a marker.
pub(super) fn truncate_middle(text: String, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text;
    }
    let head_len = max_chars * 3 / 5;
    let tail_len = max_chars - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(total - tail_len).collect();
    format!(
        "{head}\n[... {} characters omitted ...]\n{tail}",
        total - head_len - tail_len
    )
}

/// A tool's JSON output as the text the model sees: strings as-is,
/// anything else as compact JSON.
fn output_text(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The outcome of one round: a tool-result message covering every call
/// (in the model's order), and whether the turn was cancelled partway —
/// in which case calls that did run keep their real results and the rest
/// say they didn't run.
pub(super) struct RoundOutcome {
    pub message: Message,
    pub cancelled: bool,
}

pub(super) async fn run_round(
    agent: &Agent,
    turn_id: TurnId,
    calls: Vec<RequestedToolCall>,
    cancel: &CancellationToken,
) -> RoundOutcome {
    let registry = agent.registry_snapshot();
    let mut slots = Vec::with_capacity(calls.len());
    let mut cancelled = false;

    // Phase 1, in order: hooks and approval. One prompt at a time, in the
    // order the model asked.
    for call in &calls {
        if cancel.is_cancelled() {
            cancelled = true;
            break;
        }
        match approve_call(agent, &registry, turn_id, call, cancel).await {
            Some(slot) => slots.push(slot),
            None => {
                cancelled = true;
                break;
            }
        }
    }

    // Phase 2: run what was approved — unless the turn was cancelled while
    // approving. Consecutive parallel-safe calls run together; anything
    // else runs alone, in order.
    let mut results: Vec<Option<(String, bool)>> = vec![None; calls.len()];
    let mut pending = Vec::new();
    for (i, slot) in slots.into_iter().enumerate() {
        match slot {
            Slot::Done { text, is_error } => results[i] = Some((text, is_error)),
            Slot::Approved { authorized, id } => pending.push((i, authorized, id)),
        }
    }
    if !cancelled {
        let mut batch = Vec::new();
        for (i, authorized, id) in pending {
            if cancelled {
                break;
            }
            if authorized.parallel_safe() {
                batch.push((i, authorized, id));
                continue;
            }
            cancelled = run_batch(
                agent,
                turn_id,
                std::mem::take(&mut batch),
                cancel,
                &mut results,
            )
            .await;
            if !cancelled {
                cancelled = run_batch(
                    agent,
                    turn_id,
                    vec![(i, authorized, id)],
                    cancel,
                    &mut results,
                )
                .await;
            }
        }
        if !cancelled {
            cancelled = run_batch(agent, turn_id, batch, cancel, &mut results).await;
        }
    }

    let blocks = calls
        .iter()
        .zip(results)
        .map(|(call, result)| {
            let (text, is_error) = result.unwrap_or_else(|| {
                (
                    "not executed: the turn was cancelled before this tool call ran".to_string(),
                    true,
                )
            });
            ContentBlock::ToolResult {
                tool_use_id: call.id.clone(),
                content: vec![ContentBlock::text(truncate_middle(
                    text,
                    agent.settings.max_tool_output_chars,
                ))],
                is_error,
            }
        })
        .collect();
    RoundOutcome {
        message: Message::with_blocks(Role::Tool, blocks),
        cancelled,
    }
}

/// Hooks + approval for one call. `None` means the turn was cancelled
/// while waiting for a human; everything else (veto, denial, unknown tool)
/// becomes a result the model sees.
async fn approve_call(
    agent: &Agent,
    registry: &arbe_tools::ToolRegistry,
    turn_id: TurnId,
    call: &RequestedToolCall,
    cancel: &CancellationToken,
) -> Option<Slot> {
    let hook_result = hooks::run(
        &agent.hooks,
        HookPhase::BeforeToolExecute,
        &ToolCallPayload {
            turn_id: turn_id.to_string(),
            tool_name: &call.name,
            arguments: &call.arguments,
        },
    )
    .await;
    let risk = registry.risk_of(&call.name);
    let invocation = ToolInvocation {
        id: ToolCallId::new(),
        source_turn: turn_id,
        tool_name: call.name.clone(),
        arguments: call.arguments.clone(),
        risk,
        rationale: None,
    };
    let id = invocation.id;
    let arguments = match hooks::tool_call_verdict(&hook_result, &call.arguments) {
        ToolCallVerdict::Proceed(arguments) => arguments,
        ToolCallVerdict::Veto(reason) => {
            return Some(denied(
                agent,
                turn_id,
                id,
                &call.name,
                format!("blocked by hook: {reason}"),
            ));
        }
    };
    let invocation = ToolInvocation {
        arguments: arguments.clone(),
        ..invocation
    };
    agent.events.publish(RuntimeEvent::ToolCallProposed {
        turn_id,
        tool_call_id: id,
        tool_name: call.name.clone(),
        arguments,
        risk,
    });

    let gate = |invocation, decision| {
        authorize(
            registry,
            agent.policy.as_ref(),
            &agent.approval_ctx,
            invocation,
            decision,
        )
    };
    let (authorization, decided_by_human) = match gate(invocation, None) {
        Ok(Authorization::NeedsHuman(invocation)) => {
            agent.events.publish(RuntimeEvent::ToolApprovalRequested {
                turn_id,
                tool_call_id: id,
            });
            let rx = agent.decisions.register(id);
            let decision = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    agent.decisions.withdraw(id);
                    return None;
                }
                decision = rx => decision.unwrap_or(ApprovalDecision::DeniedOnce),
            };
            (gate(invocation, Some(decision)), true)
        }
        other => (other, false),
    };

    Some(match authorization {
        Ok(Authorization::Approved(authorized)) => Slot::Approved { authorized, id },
        Ok(Authorization::Denied(_)) => {
            let reason = if decided_by_human {
                "denied by the user"
            } else {
                "denied by approval policy"
            };
            denied(agent, turn_id, id, &call.name, reason.to_string())
        }
        // `NeedsHuman` after a human decision can't happen; treat it as
        // denied rather than looping.
        Ok(Authorization::NeedsHuman(_)) => {
            denied(agent, turn_id, id, &call.name, "no decision".to_string())
        }
        Err(err) => {
            let message = err.to_string();
            publish_executed(
                agent,
                turn_id,
                id,
                &call.name,
                json!({ "error": message }),
                true,
            );
            Slot::Done {
                text: message,
                is_error: true,
            }
        }
    })
}

fn denied(agent: &Agent, turn_id: TurnId, id: ToolCallId, tool_name: &str, reason: String) -> Slot {
    agent.events.publish(RuntimeEvent::ToolCallDenied {
        turn_id,
        tool_call_id: id,
        tool_name: tool_name.to_string(),
        reason: reason.clone(),
    });
    Slot::Done {
        text: reason,
        is_error: true,
    }
}

fn publish_executed(
    agent: &Agent,
    turn_id: TurnId,
    id: ToolCallId,
    tool_name: &str,
    output: Value,
    is_error: bool,
) {
    agent.events.publish(RuntimeEvent::ToolExecuted {
        turn_id,
        tool_call_id: id,
        tool_name: tool_name.to_string(),
        result: ToolResult {
            id,
            output,
            is_error,
        },
    });
}

/// Runs a batch of approved calls concurrently and stores each result at
/// its slot index. Returns whether the turn was cancelled — checked once
/// the whole batch has settled, since the other tools see the same token
/// and stop too, and every result that did come back is kept.
async fn run_batch(
    agent: &Agent,
    turn_id: TurnId,
    batch: Vec<(usize, Authorized, ToolCallId)>,
    cancel: &CancellationToken,
    results: &mut [Option<(String, bool)>],
) -> bool {
    if batch.is_empty() {
        return cancel.is_cancelled();
    }
    let runs = batch.into_iter().map(|(index, authorized, id)| async move {
        let tool_name = authorized.invocation().tool_name.clone();
        let events = agent.events.clone();
        let tool_ctx = ToolContext {
            cancel: cancel.clone(),
            progress: Some(Arc::new(move |update: String| {
                events.publish(RuntimeEvent::ToolProgress {
                    turn_id,
                    tool_call_id: id,
                    update,
                })
            })),
        };
        let outcome = authorized.execute(&tool_ctx).await;
        (index, id, tool_name, outcome)
    });

    let mut cancelled = false;
    for (index, id, tool_name, outcome) in join_all(runs).await {
        let (text, is_error) = match outcome {
            Ok(result) => {
                let text = output_text(&result.output);
                publish_executed(
                    agent,
                    turn_id,
                    id,
                    &tool_name,
                    result.output,
                    result.is_error,
                );
                (text, result.is_error)
            }
            Err(ToolError::Cancelled) => {
                cancelled = true;
                ("cancelled".to_string(), true)
            }
            Err(err) => {
                let message = err.to_string();
                publish_executed(
                    agent,
                    turn_id,
                    id,
                    &tool_name,
                    json!({ "error": message }),
                    true,
                );
                (message, true)
            }
        };
        hooks::run(
            &agent.hooks,
            HookPhase::AfterToolExecute,
            &ToolResultPayload {
                turn_id: turn_id.to_string(),
                tool_name: &tool_name,
                is_error,
                output_chars: text.chars().count(),
            },
        )
        .await;
        results[index] = Some((text, is_error));
    }
    cancelled || cancel.is_cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_untouched_and_long_output_keeps_head_and_tail() {
        assert_eq!(truncate_middle("hello".into(), 10), "hello");
        let long: String = ('a'..='z').cycle().take(1_000).collect();
        let cut = truncate_middle(long.clone(), 100);
        assert!(cut.starts_with(&long[..60]));
        assert!(cut.ends_with(&long[960..]));
        assert!(cut.contains("[... 900 characters omitted ...]"));
    }

    #[test]
    fn string_output_is_passed_as_text_and_other_json_is_serialized() {
        assert_eq!(output_text(&json!("plain")), "plain");
        assert_eq!(output_text(&json!({"a": 1})), r#"{"a":1}"#);
    }
}
