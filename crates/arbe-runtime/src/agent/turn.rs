//! One turn: context assembly, the model loop (streaming rounds with tool
//! rounds in between), loop guards, and persistence of every message as
//! it's produced.

use arbe_core::{
    ContentBlock, HarnessError, LoopMachine, LoopPhase, MemoryError, Message, ProviderError,
    RequestedToolCall, Role, RuntimeEvent, StopReason, ToolSpec, Turn, TurnId, Usage,
};
use arbe_hooks::HookPhase;
use arbe_memory::HistoryEntry;
use arbe_providers::{
    AccumulatedResponse, CancellationToken, ModelRequest, ProviderEvent, ResponseAccumulator,
    stream_with_retry,
};
use arbe_storage::InFlightMessage;
use futures_util::StreamExt;

use super::hooks::{self, ErrorPayload, ModelCallPayload, ModelResultPayload, TurnPayload};
use super::{Agent, build_system_prompt_async, compaction, tools};

/// How many consecutive rounds may request the identical set of tool calls
/// before the turn is stopped as stuck.
const MAX_IDENTICAL_ROUNDS: u32 = 3;

/// Moves the loop to `to`, turning an illegal transition into a
/// `HarnessError::Internal` instead of a panic. It's still a harness bug,
/// so debug builds assert loudly; release builds fail just this turn.
fn advance(machine: &mut LoopMachine, to: LoopPhase) -> Result<(), HarnessError> {
    machine.transition(to).map(|_| ()).map_err(|err| {
        debug_assert!(false, "{err}");
        HarnessError::Internal(err.to_string())
    })
}

/// A cancelled provider request cancels the turn; anything else is a
/// provider failure.
fn provider_error(err: ProviderError) -> HarnessError {
    match err {
        ProviderError::Cancelled => HarnessError::Cancelled,
        other => HarnessError::Provider(other),
    }
}

/// Gives every tool call in `messages` that never got a result (the turn
/// was cancelled, a guard stopped it, or the process died) an error result,
/// placed right where the provider expects it: after the calls, before the
/// next non-tool message. Keeps the history a valid call/result sequence
/// for every provider.
///
/// Matching is positional, not global: some servers reuse call ids
/// (`call_0`) in every response, so a result only answers a call from the
/// assistant message immediately before it.
pub(super) fn close_dangling_tool_uses(messages: &mut Vec<Message>) {
    fn closure(pending: Vec<RequestedToolCall>) -> Message {
        Message::with_blocks(
            Role::Tool,
            pending
                .into_iter()
                .map(|call| ContentBlock::ToolResult {
                    tool_use_id: call.id,
                    content: vec![ContentBlock::text(
                        "not executed: the turn ended before this tool call ran",
                    )],
                    is_error: true,
                })
                .collect(),
        )
    }

    let mut out = Vec::with_capacity(messages.len() + 1);
    let mut pending: Vec<RequestedToolCall> = Vec::new();
    for message in messages.drain(..) {
        if message.role == Role::Tool {
            for block in &message.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block
                    && let Some(i) = pending.iter().position(|c| &c.id == tool_use_id)
                {
                    pending.remove(i);
                }
            }
        } else if !pending.is_empty() {
            out.push(closure(std::mem::take(&mut pending)));
        }
        if message.role == Role::Assistant {
            pending = message.tool_uses();
        }
        out.push(message);
    }
    if !pending.is_empty() {
        out.push(closure(pending));
    }
    *messages = out;
}

/// Drops tool calls from a message cut off mid-stream: their arguments may
/// be incomplete, and they were never going to run.
fn without_tool_uses(mut message: Message) -> Message {
    message
        .content
        .retain(|b| !matches!(b, ContentBlock::ToolUse { .. }));
    message
}

/// How the model loop ended (short of an error).
struct LoopEnd {
    stop_reason: StopReason,
}

struct TurnRunner<'a> {
    agent: &'a Agent,
    turn: Turn,
    cancel: &'a CancellationToken,
    machine: LoopMachine,
    /// Every message this turn added, in order (user message first).
    trace: Vec<Message>,
    usage: Usage,
}

pub(super) async fn run_turn(
    agent: &Agent,
    content: String,
    cancel: &CancellationToken,
) -> Result<String, HarnessError> {
    let turn = {
        let state = agent.state();
        Turn::new(state.meta.id, state.next_turn_index)
    };
    let turn_id = turn.id;
    let mut runner = TurnRunner {
        agent,
        turn,
        cancel,
        machine: LoopMachine::new(),
        trace: Vec::new(),
        usage: Usage::default(),
    };

    match runner.execute(content).await {
        Ok(end) => {
            let answer = runner.final_answer();
            runner.commit(end.stop_reason.clone())?;
            advance(&mut runner.machine, LoopPhase::EmitEvents)?;
            agent.events.publish(RuntimeEvent::TurnCompleted {
                session_id: agent.session_id(),
                turn_id,
                stop_reason: end.stop_reason,
            });
            advance(&mut runner.machine, LoopPhase::Idle)?;
            hooks::run(
                &agent.hooks,
                &agent.events,
                HookPhase::OnTurnComplete,
                &TurnPayload {
                    turn_id: turn_id.to_string(),
                },
            )
            .await;
            Ok(answer)
        }
        Err(HarnessError::Cancelled) => {
            runner.commit(StopReason::Cancelled)?;
            agent.events.publish(RuntimeEvent::TurnCancelled {
                session_id: agent.session_id(),
                turn_id,
            });
            Err(HarnessError::Cancelled)
        }
        Err(err) => {
            // Keep the turn if it did anything beyond receiving the user's
            // message (tools may have had side effects the next turn needs
            // to know about); otherwise drop it so a retry doesn't repeat
            // the question in history.
            if runner.trace.len() > 1 {
                runner.commit(StopReason::Other(format!("error: {err}")))?;
            } else {
                runner.discard();
            }
            agent.events.publish(RuntimeEvent::RuntimeError {
                turn_id: Some(turn_id),
                reason: err.to_string(),
            });
            hooks::run(
                &agent.hooks,
                &agent.events,
                HookPhase::OnError,
                &ErrorPayload {
                    turn_id: turn_id.to_string(),
                    error: err.to_string(),
                },
            )
            .await;
            Err(err)
        }
    }
}

impl TurnRunner<'_> {
    fn turn_id(&self) -> TurnId {
        self.turn.id
    }

    /// Adds a message to the turn and writes it to the in-flight log, so a
    /// crash from here on doesn't lose it.
    fn record(&mut self, message: Message) {
        let entry = InFlightMessage {
            turn_id: self.turn.id,
            turn_index: self.turn.index,
            message: message.clone(),
        };
        if let Err(err) = self
            .agent
            .store
            .append_in_flight(self.agent.session_id(), &entry)
        {
            // The turn still completes and is persisted normally; only
            // crash recovery for it is lost.
            tracing::warn!(%err, "failed to write in-flight message");
        }
        self.trace.push(message);
    }

    /// The text of the turn's last assistant message.
    fn final_answer(&self) -> String {
        self.trace
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(Message::text)
            .unwrap_or_default()
    }

    /// Persists the turn and folds it into session state.
    fn commit(&mut self, stop_reason: StopReason) -> Result<(), HarnessError> {
        let agent = self.agent;
        close_dangling_tool_uses(&mut self.trace);
        let mut turn = self.turn.clone();
        turn.messages = self.trace.clone();
        turn.usage = self.usage;
        turn.stop_reason = Some(stop_reason);
        agent
            .store
            .append_turn(&turn)
            .map_err(|e| HarnessError::Memory(MemoryError::StoreUnavailable(e.to_string())))?;
        if let Err(err) = agent.store.clear_in_flight(agent.session_id()) {
            // Harmless: recovery sees the turn is already committed.
            tracing::warn!(%err, "failed to clear in-flight log");
        }

        let session_usage = {
            let mut state = agent.state();
            state
                .history
                .extend(turn.messages.iter().map(|message| HistoryEntry {
                    turn_index: turn.index,
                    message: message.clone(),
                }));
            state.next_turn_index = turn.index + 1;
            state.meta.usage += turn.usage;
            state.meta.touch(arbe_core::SessionStatus::Active);
            if let Err(err) = agent.store.save_meta(&state.meta) {
                // The turn itself is safely persisted; only meta.json's
                // running usage total/timestamp is stale.
                tracing::warn!(%err, "failed to update session metadata");
            }
            state.meta.usage
        };
        agent.events.publish(RuntimeEvent::UsageUpdated {
            session_id: agent.session_id(),
            turn_id: turn.id,
            turn: turn.usage,
            session: session_usage,
        });
        Ok(())
    }

    /// Abandons the turn without persisting it.
    fn discard(&mut self) {
        if let Err(err) = self.agent.store.clear_in_flight(self.agent.session_id()) {
            tracing::warn!(%err, "failed to clear in-flight log");
        }
    }

    async fn execute(&mut self, content: String) -> Result<LoopEnd, HarnessError> {
        let agent = self.agent;
        let turn_id = self.turn_id();
        advance(&mut self.machine, LoopPhase::ReceiveUserInput)?;
        agent.events.publish(RuntimeEvent::TurnStarted {
            session_id: agent.session_id(),
            turn_id,
        });
        hooks::run(
            &agent.hooks,
            &agent.events,
            HookPhase::BeforeContextAssembly,
            &TurnPayload {
                turn_id: turn_id.to_string(),
            },
        )
        .await;

        advance(&mut self.machine, LoopPhase::AssembleContext)?;
        if agent.settings.auto_compact {
            match compaction::compact(agent, false, self.cancel).await {
                Ok(_) => {}
                Err(HarnessError::Cancelled) => return Err(HarnessError::Cancelled),
                // Not fatal: the history strategy still trims to fit.
                Err(err) => tracing::warn!(%err, "compaction failed; trimming history instead"),
            }
        }
        let system_prompt =
            build_system_prompt_async(&agent.settings.prompt, &agent.settings.project_dir).await;
        let user_message = Message::new(Role::User, content);
        let (context, estimated_display) = {
            let mut state = agent.state();
            state.pipeline.system_instructions = vec![system_prompt];
            state.pipeline.conversation_summary = state.summary.as_ref().map(|c| c.summary.clone());
            // The pipeline budgets in estimator units; convert so the real
            // prompt lands inside the budget.
            let budget = state
                .calibration
                .budget_in_estimate_units(agent.settings.budget_tokens);
            let context = state.pipeline.assemble(
                agent.strategy.as_ref(),
                &state.history,
                &state.pinned_turn_indices,
                user_message.clone(),
                budget,
            );
            let display = state.calibration.calibrate(context.estimated_tokens);
            state.last_estimated_tokens = display;
            (context, display)
        };
        agent.events.publish(RuntimeEvent::ContextBuilt {
            turn_id,
            estimated_tokens: estimated_display,
        });
        self.record(user_message);

        advance(&mut self.machine, LoopPhase::PlanOrDirectRespond)?;
        agent.refresh_mcp_tools().await;
        let tool_specs = if agent
            .provider
            .capabilities(&agent.settings.model)
            .tool_calls
        {
            agent.registry_snapshot().specs()
        } else {
            Vec::new()
        };

        self.model_loop(context.messages, context.estimated_tokens, tool_specs)
            .await
    }

    async fn model_loop(
        &mut self,
        mut messages: Vec<Message>,
        estimated_prompt_tokens: u64,
        tool_specs: Vec<ToolSpec>,
    ) -> Result<LoopEnd, HarnessError> {
        let agent = self.agent;
        let turn_id = self.turn_id();
        let mut previous_calls: Option<Vec<(String, String)>> = None;
        let mut identical_rounds = 1;

        for round in 0..agent.settings.max_tool_rounds {
            // Checked from the second round on: nothing has been spent
            // before the first, and the loop can only stop after a round.
            if round > 0
                && let Some(limit) = agent.settings.max_turn_tokens
                && self.usage.total_tokens() >= limit
            {
                advance(&mut self.machine, LoopPhase::PersistTurn)?;
                return Ok(LoopEnd {
                    stop_reason: StopReason::TurnTokenLimit,
                });
            }

            advance(&mut self.machine, LoopPhase::ModelInference)?;
            hooks::run(
                &agent.hooks,
                &agent.events,
                HookPhase::BeforeModelCall,
                &ModelCallPayload {
                    turn_id: turn_id.to_string(),
                    round,
                    message_count: messages.len(),
                },
            )
            .await;

            // A long tool loop grows the turn's own messages without bound;
            // stub out its older tool results once they no longer fit
            // (the persisted trace keeps them in full). The newest results
            // — the ones the model hasn't read yet — are never touched.
            if round > 0 {
                let budget = agent
                    .state()
                    .calibration
                    .budget_in_estimate_units(agent.settings.budget_tokens);
                let pruned = arbe_memory::prune_tool_results(&mut messages, budget, 1);
                if pruned > 0 {
                    tracing::debug!(pruned, "stubbed old tool output within the turn");
                }
            }

            let request = ModelRequest {
                model: agent.settings.model.clone(),
                messages: messages.clone(),
                temperature: agent.settings.temperature,
                max_tokens: agent.settings.max_tokens,
                tools: tool_specs.clone(),
                thinking_budget_tokens: agent.settings.thinking_budget_tokens,
            };
            let (response, cancelled) = self.stream_inference(request).await?;
            self.usage += response.usage;
            if round == 0 {
                // Only the first round's prompt is exactly what was
                // estimated; later rounds add the tool trace.
                let actual = response.usage.input_tokens
                    + response.usage.cache_read_tokens
                    + response.usage.cache_write_tokens;
                agent
                    .state()
                    .calibration
                    .observe(estimated_prompt_tokens, actual);
            }
            let tool_calls = response.message.tool_uses();

            hooks::run(
                &agent.hooks,
                &agent.events,
                HookPhase::AfterModelCall,
                &ModelResultPayload {
                    turn_id: turn_id.to_string(),
                    round,
                    text_chars: response.message.text().chars().count(),
                    tool_calls: tool_calls.len(),
                },
            )
            .await;
            advance(&mut self.machine, LoopPhase::InterpretOutput)?;

            if cancelled {
                let partial = without_tool_uses(response.message);
                if !partial.content.is_empty() {
                    self.record(partial);
                }
                return Err(HarnessError::Cancelled);
            }

            self.record(response.message.clone());
            messages.push(response.message);

            if tool_calls.is_empty() {
                advance(&mut self.machine, LoopPhase::PersistTurn)?;
                return Ok(LoopEnd {
                    stop_reason: response.stop_reason,
                });
            }

            let signature = call_signature(&tool_calls);
            if previous_calls.as_ref() == Some(&signature) {
                identical_rounds += 1;
            } else {
                identical_rounds = 1;
            }
            if identical_rounds >= MAX_IDENTICAL_ROUNDS {
                advance(&mut self.machine, LoopPhase::PersistTurn)?;
                return Ok(LoopEnd {
                    stop_reason: StopReason::RepeatedToolCall,
                });
            }
            previous_calls = Some(signature);

            advance(&mut self.machine, LoopPhase::ToolApproval)?;
            let round_outcome = tools::run_round(agent, turn_id, tool_calls, self.cancel).await;
            self.record(round_outcome.message.clone());
            if round_outcome.cancelled {
                return Err(HarnessError::Cancelled);
            }
            messages.push(round_outcome.message);
            advance(&mut self.machine, LoopPhase::ToolExecution)?;
            advance(&mut self.machine, LoopPhase::PostToolReflection)?;
        }

        advance(&mut self.machine, LoopPhase::PersistTurn)?;
        Ok(LoopEnd {
            stop_reason: StopReason::ToolRoundLimit,
        })
    }

    /// Streams one inference call (retrying transient failures, each
    /// announced as `ProviderRetrying`), forwarding deltas as events.
    /// Cancellation mid-stream isn't an error here: it returns what arrived
    /// so far with `cancelled = true`, so partial output can be kept.
    async fn stream_inference(
        &self,
        request: ModelRequest,
    ) -> Result<(AccumulatedResponse, bool), HarnessError> {
        let agent = self.agent;
        let turn_id = self.turn_id();
        let events = &agent.events;
        let mut stream = stream_with_retry(
            agent.provider.as_ref(),
            request,
            self.cancel,
            &agent.settings.retry,
            |notice| {
                events.publish(RuntimeEvent::ProviderRetrying {
                    turn_id,
                    attempt: notice.attempt,
                    delay_ms: notice.delay.as_millis() as u64,
                    reason: notice.reason.clone(),
                })
            },
        )
        .await
        .map_err(provider_error)?;

        let mut acc = ResponseAccumulator::new();
        while let Some(event) = stream.next().await {
            let event = match event {
                Ok(event) => event,
                Err(ProviderError::Cancelled) => return Ok((acc.finish(), true)),
                Err(err) => return Err(HarnessError::Provider(err)),
            };
            match &event {
                ProviderEvent::TextDelta(delta) => events.publish(RuntimeEvent::ModelStreamChunk {
                    turn_id,
                    delta: delta.clone(),
                }),
                ProviderEvent::ThinkingDelta(delta) => {
                    events.publish(RuntimeEvent::ThinkingDelta {
                        turn_id,
                        delta: delta.clone(),
                    })
                }
                ProviderEvent::ToolUseStart { id, name } => {
                    events.publish(RuntimeEvent::ToolUseStarted {
                        turn_id,
                        provider_call_id: id.clone(),
                        tool_name: name.clone(),
                    })
                }
                ProviderEvent::ToolUseInputDelta { id, partial_json } => {
                    events.publish(RuntimeEvent::ToolUseInputDelta {
                        turn_id,
                        provider_call_id: id.clone(),
                        partial_json: partial_json.clone(),
                    })
                }
                _ => {}
            }
            acc.push(event);
        }
        Ok((acc.finish(), false))
    }
}

/// A round's tool calls, reduced to what makes two rounds "the same
/// request": names and arguments, in order (provider call ids differ every
/// round).
fn call_signature(calls: &[RequestedToolCall]) -> Vec<(String, String)> {
    calls
        .iter()
        .map(|c| (c.name.clone(), c.arguments.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str) -> RequestedToolCall {
        RequestedToolCall {
            id: id.into(),
            name: "t".into(),
            arguments: json!({}),
        }
    }

    fn result_ids(message: &Message) -> Vec<String> {
        message
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn answered_calls_are_left_alone() {
        let mut messages = vec![
            Message::new(Role::User, "q"),
            Message::assistant_tool_calls(vec![call("a")]),
            Message::tool_result("a", "ok"),
        ];
        close_dangling_tool_uses(&mut messages);
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn unanswered_calls_get_results_right_after_them() {
        let mut messages = vec![
            Message::new(Role::User, "q"),
            Message::assistant_tool_calls(vec![call("a"), call("b")]),
            Message::tool_result("a", "ok"),
            Message::new(Role::User, "next"),
        ];
        close_dangling_tool_uses(&mut messages);
        assert_eq!(messages.len(), 5);
        assert_eq!(result_ids(&messages[3]), vec!["b"]);
        assert_eq!(messages[4].text(), "next");
    }

    #[test]
    fn a_reused_call_id_is_matched_by_position_not_globally() {
        let mut messages = vec![
            Message::assistant_tool_calls(vec![call("call_0")]),
            Message::tool_result("call_0", "ok"),
            Message::assistant_tool_calls(vec![call("call_0")]),
        ];
        close_dangling_tool_uses(&mut messages);
        assert_eq!(messages.len(), 4);
        assert_eq!(result_ids(&messages[3]), vec!["call_0"]);
    }
}
