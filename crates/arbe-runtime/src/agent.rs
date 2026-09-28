use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arbe_core::{
    ApprovalDecision, ContentBlock, EventEnvelope, HarnessError, LoopMachine, LoopPhase,
    MemoryError, Message, ProviderError, RiskLevel, Role, RuntimeEvent, SessionId, SessionMeta,
    SessionStatus, StopReason, ToolCallId, ToolError, ToolInvocation, ToolResult, ToolSpec, Turn,
    TurnId, Usage,
};
use arbe_hooks::{HookPhase, HookRegistry};
use arbe_memory::{
    CompactWithSummaryStrategy, ContextPipeline, ContextStrategy, HistoryEntry, TokenCalibration,
    TruncationStrategy,
};
use arbe_providers::{
    AccumulatedResponse, CancellationToken, ModelProvider, ModelRequest, ProviderEvent,
    ProviderRegistry, ProviderSettings, ResponseAccumulator, RetryPolicy, stream_with_retry,
};
use arbe_skills::SkillScope;
use arbe_storage::SessionStore;
use arbe_tools::{
    ApprovalContext, ApprovalPolicy, GatedOutcome, StandardApprovalPolicy, ToolContext,
    ToolExecutor, ToolRegistry, execute_gated,
};
use futures_util::StreamExt;
use serde_json::json;

use crate::EventBus;
use crate::config::RuntimeConfig;

/// A mailbox for human decisions on tool calls currently paused mid-turn
/// inside `Agent::run_tool_loop` (via `resolve_gated_call`), reachable
/// *without* locking the `Agent` itself.
///
/// This has to be separate from `Agent` (rather than a plain field a
/// caller reaches through `&mut Agent`): a caller normally holds the
/// `Agent` behind a single `tokio::sync::Mutex` (the TUI does — see
/// `arbe-tui`) and calls `submit_message` through it; `submit_message`
/// holds that lock for the *entire* turn, including the moment it's
/// suspended awaiting a human decision. If supplying that decision also
/// required the same lock, it could never be acquired — the in-flight
/// turn is holding it precisely because it's waiting on the thing only
/// that lock would let you send. `ToolDecisions` is `Clone` (cheaply —
/// it's an `Arc` around a small, uncontended `std::sync::Mutex`, so a
/// caller can hold its own copy (via `Agent::tool_decisions`) alongside
/// the `Agent` lock, not behind it.
#[derive(Clone, Default)]
pub struct ToolDecisions {
    inner:
        Arc<std::sync::Mutex<HashMap<ToolCallId, tokio::sync::oneshot::Sender<ApprovalDecision>>>>,
}

impl ToolDecisions {
    fn register(&self, id: ToolCallId) -> tokio::sync::oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.inner
            .lock()
            .expect("tool decisions mutex poisoned")
            .insert(id, tx);
        rx
    }

    /// Supplies a decision for `id`. Returns `false` if nothing is (or is
    /// no longer — e.g. already timed out) waiting on it, which a caller
    /// can use to fall back to a different resolution path (see
    /// `arbe-tui`'s dispatch between this and the manual `/tool` demo
    /// path's `Agent::resolve_tool_call`).
    pub fn supply(&self, id: ToolCallId, decision: ApprovalDecision) -> bool {
        match self
            .inner
            .lock()
            .expect("tool decisions mutex poisoned")
            .remove(&id)
        {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }
}

/// Moves the loop to `to`, turning an illegal transition into a
/// `HarnessError::Internal` instead of a panic. It's still a harness bug,
/// so debug builds assert loudly; release builds fail just this turn.
fn advance(machine: &mut LoopMachine, to: LoopPhase) -> Result<(), HarnessError> {
    machine.transition(to).map(|_| ()).map_err(|err| {
        debug_assert!(false, "{err}");
        HarnessError::Internal(err.to_string())
    })
}

fn build_strategy(name: &str) -> Box<dyn ContextStrategy> {
    match name {
        "compact_summary" => Box::new(CompactWithSummaryStrategy),
        _ => Box::new(TruncationStrategy),
    }
}

/// Reads `<arbe_home>/instructions/agent.md` and
/// `<project_dir>/agent.md`/`CLAUDE.md`, then renders them into the system
/// prompt template (`crate::system_prompt`). Re-read on every turn (see
/// `submit_message`) rather than cached at `Agent::assemble`, so edits to
/// either file take effect on the next turn without restarting the
/// session. A read error degrades to an absent section — same
/// skills-are-additive-not-load-bearing reasoning as
/// `load_global_skill_instructions`, so a transient/permission error on an
/// instructions file can't take down a turn.
fn build_system_prompt(project_dir: &std::path::Path) -> String {
    let global = arbe_storage::instructions::read_global_instructions().unwrap_or_else(|err| {
        tracing::warn!(%err, "failed to read global instructions; continuing without them");
        None
    });
    let project = arbe_storage::instructions::read_project_instructions(project_dir)
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "failed to read project instructions; continuing without them");
            None
        });
    crate::system_prompt::render_system_prompt(global.as_deref(), project.as_deref())
}

/// Same as [`build_system_prompt`], but for the `submit_message` hot path
/// (re-read on every turn, see that function's doc comment) — `arbe_storage`
/// has no `tokio` dependency, so its instructions readers are synchronous
/// `std::fs` calls; running them directly in an `async fn` would block the
/// executor's worker thread for the duration of two disk reads instead of
/// yielding. `spawn_blocking` moves that work to a thread meant for it.
async fn build_system_prompt_async(project_dir: &std::path::Path) -> String {
    let project_dir = project_dir.to_path_buf();
    tokio::task::spawn_blocking(move || build_system_prompt(&project_dir))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "system prompt render task panicked; using template with no instructions");
            crate::system_prompt::render_system_prompt(None, None)
        })
}

/// Loads global skills from `~/.arbe/skills/` (harness spec FR-6) and
/// returns their instruction bodies, ready to fold into a
/// `ContextPipeline`. A missing/unreadable skills directory degrades to no
/// skills rather than failing agent construction — skills are additive,
/// not load-bearing (mirrors how a missing MCP server is handled).
fn load_global_skill_instructions() -> Vec<String> {
    match arbe_skills::load_dir(&arbe_storage::paths::skills_dir(), SkillScope::Global) {
        Ok(global) => arbe_skills::merge_skills(Vec::new(), Vec::new(), global)
            .into_iter()
            .map(|m| m.instructions)
            .collect(),
        Err(err) => {
            tracing::warn!(%err, "failed to load global skills; continuing without them");
            Vec::new()
        }
    }
}

/// What a turn's model loop produced.
struct TurnOutcome {
    /// The final answer's text.
    text: String,
    /// Usage summed over every inference call in the turn.
    usage: Usage,
    stop_reason: StopReason,
}

/// A cancelled provider request cancels the turn; anything else is a
/// provider failure.
fn provider_error(err: ProviderError) -> HarnessError {
    match err {
        ProviderError::Cancelled => HarnessError::Cancelled,
        other => HarnessError::Provider(other),
    }
}

/// The composed agent loop: one turn is context assembly -> inference ->
/// (no tool-call parsing yet — see the note on `submit_message`) ->
/// persistence -> events, driven through `LoopMachine` so illegal phase
/// skips panic loudly in development rather than silently corrupting a
/// turn.
pub struct Agent {
    store: SessionStore,
    meta: SessionMeta,
    provider: Box<dyn ModelProvider>,
    pipeline: ContextPipeline,
    strategy: Box<dyn ContextStrategy>,
    budget_tokens: u64,
    /// Runaway guard for `run_model_loop` — see `RuntimeConfig::max_tool_rounds`.
    max_tool_rounds: u32,
    retry: RetryPolicy,
    /// See `RuntimeConfig::thinking_budget_tokens`.
    thinking_budget_tokens: Option<u64>,
    temperature: f32,
    max_tokens: u64,
    registry: ToolRegistry,
    policy: Box<dyn ApprovalPolicy>,
    approval_ctx: ApprovalContext,
    hooks: HookRegistry,
    events: Arc<EventBus>,
    history: Vec<HistoryEntry>,
    pinned_turn_indices: Vec<u64>,
    next_turn_index: u64,
    pending_tool_calls: HashMap<ToolCallId, ToolInvocation>,
    /// Mailbox for human decisions on model-initiated tool calls paused
    /// mid-turn — see `ToolDecisions`'s doc comment for why this can't
    /// just be a plain field reached through `&mut self`.
    tool_decisions: ToolDecisions,
    last_estimated_tokens: u64,
    /// Corrects the chars/4 token estimate from provider-reported input
    /// counts; budgets and displayed estimates go through it.
    token_calibration: TokenCalibration,
    /// The repo/project this agent works on — see `RuntimeConfig::project_dir`.
    /// This is the sandbox root every builtin filesystem/execute tool is
    /// registered against in `assemble`.
    project_dir: std::path::PathBuf,
}

impl Agent {
    fn assemble(
        config: &RuntimeConfig,
        store: SessionStore,
        meta: SessionMeta,
        events: Arc<EventBus>,
        history: Vec<HistoryEntry>,
    ) -> Result<Self, ProviderError> {
        let provider = ProviderRegistry::with_builtins().build(
            &config.provider_name,
            ProviderSettings {
                api_key: config.api_key.clone(),
                base_url: config.base_url.clone(),
                extra_headers: config.extra_headers.clone(),
                ..Default::default()
            },
        )?;

        let mut registry = ToolRegistry::new();
        arbe_tools::builtin::register_all(&mut registry, &config.project_dir);
        let budget_tokens = config
            .effective_context_budget(provider.capabilities(&config.model).max_context_tokens);

        Ok(Self {
            store,
            meta,
            provider,
            pipeline: ContextPipeline {
                system_instructions: vec![build_system_prompt(&config.project_dir)],
                skill_instructions: load_global_skill_instructions(),
                ..Default::default()
            },
            strategy: build_strategy(&config.memory_strategy),
            budget_tokens,
            max_tool_rounds: config.max_tool_rounds,
            retry: config.retry,
            thinking_budget_tokens: config.thinking_budget_tokens,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            registry,
            policy: Box::new(StandardApprovalPolicy),
            approval_ctx: ApprovalContext {
                session_approval_covers_high_risk: config.session_approval_covers_high_risk,
                ..ApprovalContext::new(
                    config.policy_mode,
                    config.allowlist.clone(),
                    config.denylist.clone(),
                )
            },
            hooks: HookRegistry::new(Duration::from_millis(config.hook_timeout_ms)),
            events,
            history,
            pinned_turn_indices: Vec::new(),
            next_turn_index: 0,
            pending_tool_calls: HashMap::new(),
            tool_decisions: ToolDecisions::default(),
            last_estimated_tokens: 0,
            token_calibration: TokenCalibration::default(),
            project_dir: config.project_dir.clone(),
        })
    }

    /// Starts a brand new session.
    pub fn create(
        config: &RuntimeConfig,
        store: SessionStore,
        events: Arc<EventBus>,
    ) -> Result<Self, ProviderError> {
        let meta = store
            .create_session(
                config.profile.clone(),
                config.provider_name.clone(),
                config.model.clone(),
            )
            .map_err(|e| ProviderError::Internal(format!("failed to create session: {e}")))?;
        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        Self::assemble(config, store, meta, events, Vec::new())
    }

    /// Resumes a previously created session, rebuilding in-memory history
    /// from the persisted `turns.jsonl` (harness spec FR-1: interrupted
    /// session recovery).
    pub fn resume(
        config: &RuntimeConfig,
        store: SessionStore,
        session_id: SessionId,
        events: Arc<EventBus>,
    ) -> Result<Self, ProviderError> {
        let meta = store
            .resume_session(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to resume session: {e}")))?;
        let turns = store
            .list_turns(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to load session history: {e}")))?;

        let mut history = Vec::new();
        let mut next_turn_index = 0;
        for turn in turns {
            // Same shape `run_turn` writes today: the user message and the
            // final answer (see `submit_message` on the tool trace).
            for m in [turn.user_message(), turn.final_assistant_message()]
                .into_iter()
                .flatten()
            {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m.clone(),
                });
            }
            next_turn_index = next_turn_index.max(turn.index + 1);
        }

        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        let mut agent = Self::assemble(config, store, meta, events, history)?;
        agent.next_turn_index = next_turn_index;
        Ok(agent)
    }

    pub fn session_id(&self) -> SessionId {
        self.meta.id
    }

    pub fn profile(&self) -> &str {
        &self.meta.profile
    }

    pub fn provider_name(&self) -> &str {
        &self.meta.provider
    }

    pub fn model(&self) -> &str {
        &self.meta.model
    }

    /// The repo/project directory this agent works on (`RuntimeConfig::project_dir`).
    pub fn project_dir(&self) -> &std::path::Path {
        &self.project_dir
    }

    /// Lets any client (the TUI, a future CLI) observe the same event
    /// stream this agent publishes to, without coupling to loop internals
    /// (TUI spec §5).
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<EventEnvelope> {
        self.events.subscribe()
    }

    /// Rough context-size estimate from the most recently assembled turn,
    /// for a status-bar display (TUI-FR-4); `0` before the first turn.
    pub fn last_estimated_tokens(&self) -> u64 {
        self.last_estimated_tokens
    }

    pub fn register_tool(&mut self, name: impl Into<String>, executor: Arc<dyn ToolExecutor>) {
        self.registry.register(name, executor);
    }

    pub fn close(&mut self) -> Result<(), arbe_storage::StorageError> {
        self.meta.touch(SessionStatus::Closed);
        self.store.save_meta(&self.meta)
    }

    /// Runs one full turn: assemble context, run the model loop
    /// (`run_model_loop` — streaming every round, looping through
    /// approval-gated tool calls when the model asks for them), persist,
    /// emit events.
    ///
    /// The intermediate tool-calling exchange (the assistant's tool-call
    /// requests and each tool's result) lives only in this turn's local
    /// `messages` — it is **not** yet persisted to `history`/`turns.jsonl`
    /// or replayed into a future turn's context. Only the user message and
    /// the final assistant answer are. Persisting the full trace needs the
    /// memory strategies to keep tool-use/tool-result pairs together when
    /// trimming (a lone half of a pair is rejected by providers) — v2 plan
    /// P3.3.
    ///
    /// Any error is also published as `RuntimeEvent::RuntimeError`, so an
    /// event-only consumer learns the turn failed without needing the
    /// return value.
    pub async fn submit_message(&mut self, content: String) -> Result<String, HarnessError> {
        let turn = Turn::new(self.meta.id, self.next_turn_index);
        let turn_id = turn.id;
        let result = self.run_turn(turn, content).await;
        if let Err(err) = &result {
            self.events.publish(RuntimeEvent::RuntimeError {
                turn_id: Some(turn_id),
                reason: err.to_string(),
            });
        }
        result
    }

    async fn run_turn(&mut self, turn: Turn, content: String) -> Result<String, HarnessError> {
        let mut machine = LoopMachine::new();
        advance(&mut machine, LoopPhase::ReceiveUserInput)?;
        let turn_id = turn.id;
        self.events.publish(RuntimeEvent::TurnStarted {
            session_id: self.meta.id,
            turn_id,
        });
        // Not yet reachable from outside (turn cancellation is v2 plan
        // P3.5); threaded through inference and tools so it only needs
        // exposing.
        let cancel = CancellationToken::new();

        advance(&mut machine, LoopPhase::AssembleContext)?;
        self.pipeline.system_instructions =
            vec![build_system_prompt_async(&self.project_dir).await];
        let user_message = Message::new(Role::User, content);
        let context = self.pipeline.assemble(
            self.strategy.as_ref(),
            &self.history,
            &self.pinned_turn_indices,
            user_message.clone(),
            // The pipeline budgets in estimator units; convert so the
            // *real* prompt lands inside the budget.
            self.token_calibration
                .budget_in_estimate_units(self.budget_tokens),
        );
        self.last_estimated_tokens = self.token_calibration.calibrate(context.estimated_tokens);
        self.events.publish(RuntimeEvent::ContextBuilt {
            turn_id,
            estimated_tokens: self.last_estimated_tokens,
        });

        advance(&mut machine, LoopPhase::PlanOrDirectRespond)?;

        let tool_specs: Vec<ToolSpec> = if self.provider.capabilities(&self.meta.model).tool_calls {
            arbe_tools::builtin::tool_specs()
        } else {
            Vec::new()
        };

        let outcome = self
            .run_model_loop(
                turn_id,
                context.messages,
                context.estimated_tokens,
                tool_specs,
                &mut machine,
                &cancel,
            )
            .await?;

        let assistant_message = Message::new(Role::Assistant, outcome.text.clone());
        let mut persisted_turn = turn;
        persisted_turn.messages = vec![user_message.clone(), assistant_message.clone()];
        persisted_turn.usage = outcome.usage;
        persisted_turn.stop_reason = Some(outcome.stop_reason);
        self.store
            .append_turn(&persisted_turn)
            .map_err(|e| HarnessError::Memory(MemoryError::StoreUnavailable(e.to_string())))?;

        self.meta.usage += outcome.usage;
        self.meta.touch(SessionStatus::Active);
        if let Err(err) = self.store.save_meta(&self.meta) {
            // The turn itself is safely persisted; only the running usage
            // total/timestamp in meta.json is stale, which isn't worth
            // failing the turn over.
            tracing::warn!(%err, "failed to update session metadata");
        }
        self.events.publish(RuntimeEvent::UsageUpdated {
            session_id: self.meta.id,
            turn_id,
            turn: outcome.usage,
            session: self.meta.usage,
        });

        self.history.push(HistoryEntry {
            turn_index: persisted_turn.index,
            message: user_message,
        });
        self.history.push(HistoryEntry {
            turn_index: persisted_turn.index,
            message: assistant_message,
        });
        self.next_turn_index += 1;

        advance(&mut machine, LoopPhase::EmitEvents)?;
        self.events.publish(RuntimeEvent::TurnCompleted {
            session_id: self.meta.id,
            turn_id,
        });
        advance(&mut machine, LoopPhase::Idle)?;

        self.hooks
            .run_phase(
                HookPhase::OnTurnComplete,
                json!({ "turn_id": turn_id.to_string() }),
            )
            .await;

        Ok(outcome.text)
    }

    /// Streams one inference call (retrying transient failures per
    /// `self.retry`, each announced as `ProviderRetrying`), forwarding
    /// deltas as `RuntimeEvent`s as they arrive, and returns the
    /// accumulated response.
    async fn stream_inference(
        &self,
        turn_id: TurnId,
        request: ModelRequest,
        cancel: &CancellationToken,
    ) -> Result<AccumulatedResponse, HarnessError> {
        let events = &self.events;
        let mut stream = stream_with_retry(
            self.provider.as_ref(),
            request,
            cancel,
            &self.retry,
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
            let event = event.map_err(provider_error)?;
            match &event {
                ProviderEvent::TextDelta(delta) => {
                    self.events.publish(RuntimeEvent::ModelStreamChunk {
                        turn_id,
                        delta: delta.clone(),
                    })
                }
                ProviderEvent::ThinkingDelta(delta) => {
                    self.events.publish(RuntimeEvent::ThinkingDelta {
                        turn_id,
                        delta: delta.clone(),
                    })
                }
                ProviderEvent::ToolUseStart { id, name } => {
                    self.events.publish(RuntimeEvent::ToolUseStarted {
                        turn_id,
                        provider_call_id: id.clone(),
                        tool_name: name.clone(),
                    })
                }
                ProviderEvent::ToolUseInputDelta { id, partial_json } => {
                    self.events.publish(RuntimeEvent::ToolUseInputDelta {
                        turn_id,
                        provider_call_id: id.clone(),
                        partial_json: partial_json.clone(),
                    })
                }
                _ => {}
            }
            acc.push(event);
        }
        Ok(acc.finish())
    }

    /// Drives the model<->tool loop for one turn. Every round streams (see
    /// `stream_inference`); a round that requests no tools ends the turn.
    /// With no tools offered this is exactly one round. Every requested
    /// call passes through `execute_gated` — the same choke point the
    /// manual `/tool` path (`propose_tool_call`/`resolve_tool_call`) uses —
    /// so a model-initiated call is never less gated than a human one.
    async fn run_model_loop(
        &mut self,
        turn_id: TurnId,
        mut messages: Vec<Message>,
        estimated_prompt_tokens: u64,
        tool_specs: Vec<ToolSpec>,
        machine: &mut LoopMachine,
        cancel: &CancellationToken,
    ) -> Result<TurnOutcome, HarnessError> {
        let mut usage = Usage::default();
        for round in 0..self.max_tool_rounds {
            advance(machine, LoopPhase::ModelInference)?;
            self.hooks
                .run_phase(
                    HookPhase::BeforeModelCall,
                    json!({ "turn_id": turn_id.to_string(), "round": round, "message_count": messages.len() }),
                )
                .await;

            let request = ModelRequest {
                model: self.meta.model.clone(),
                messages: messages.clone(),
                temperature: self.temperature,
                max_tokens: self.max_tokens,
                tools: tool_specs.clone(),
                thinking_budget_tokens: self.thinking_budget_tokens,
            };
            let response = self.stream_inference(turn_id, request, cancel).await?;
            usage += response.usage;
            if round == 0 {
                // Only the first round's prompt is exactly what was
                // estimated; later rounds add the tool trace.
                let actual = response.usage.input_tokens
                    + response.usage.cache_read_tokens
                    + response.usage.cache_write_tokens;
                self.token_calibration
                    .observe(estimated_prompt_tokens, actual);
            }
            let tool_calls = response.message.tool_uses();

            self.hooks
                .run_phase(
                    HookPhase::AfterModelCall,
                    json!({ "turn_id": turn_id.to_string(), "response_len": response.message.text().len(), "tool_calls": tool_calls.len() }),
                )
                .await;

            advance(machine, LoopPhase::InterpretOutput)?;

            if tool_calls.is_empty() {
                advance(machine, LoopPhase::PersistTurn)?;
                return Ok(TurnOutcome {
                    text: response.message.text(),
                    usage,
                    stop_reason: response.stop_reason,
                });
            }

            advance(machine, LoopPhase::ToolApproval)?;
            // The whole assistant message goes back: any text the model
            // wrote alongside its tool calls is part of the conversation.
            messages.push(response.message);

            let mut any_executed = false;
            for call in tool_calls {
                let risk = arbe_tools::builtin::default_risk_for(&call.name);
                let invocation = ToolInvocation {
                    id: ToolCallId::new(),
                    source_turn: turn_id,
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    risk,
                    rationale: None,
                };
                let invocation_id = invocation.id;
                self.events.publish(RuntimeEvent::ToolCallProposed {
                    turn_id,
                    tool_call_id: invocation_id,
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    risk,
                });

                let (result_text, is_error) =
                    match self.resolve_gated_call(turn_id, invocation, cancel).await {
                        Ok(GatedOutcome::Executed(result)) => {
                            any_executed = true;
                            self.events.publish(RuntimeEvent::ToolExecuted {
                                turn_id,
                                tool_call_id: invocation_id,
                                tool_name: call.name.clone(),
                                result: result.clone(),
                            });
                            (
                                serde_json::to_string(&result.output).unwrap_or_default(),
                                result.is_error,
                            )
                        }
                        Ok(GatedOutcome::Denied) => {
                            let reason = "denied by approval policy".to_string();
                            self.events.publish(RuntimeEvent::ToolCallDenied {
                                turn_id,
                                tool_call_id: invocation_id,
                                tool_name: call.name.clone(),
                                reason: reason.clone(),
                            });
                            (reason, true)
                        }
                        // `resolve_gated_call` always resolves a pending
                        // approval before returning.
                        Ok(GatedOutcome::PendingApproval) => {
                            ("still awaiting approval".to_string(), true)
                        }
                        // Cancellation ends the turn. Any other tool failure
                        // (bad arguments, unknown tool, runtime error) goes
                        // back to the model as an error result so it can
                        // correct itself, instead of failing the whole turn.
                        Err(ToolError::Cancelled) => return Err(HarnessError::Cancelled),
                        Err(err) => {
                            any_executed = true;
                            let message = err.to_string();
                            self.events.publish(RuntimeEvent::ToolExecuted {
                                turn_id,
                                tool_call_id: invocation_id,
                                tool_name: call.name.clone(),
                                result: ToolResult {
                                    id: invocation_id,
                                    output: json!({ "error": message }),
                                    is_error: true,
                                },
                            });
                            (message, true)
                        }
                    };
                messages.push(Message::tool_result_blocks(
                    call.id,
                    vec![ContentBlock::text(result_text)],
                    is_error,
                ));
            }

            if !any_executed {
                // Every call this round was denied. `ToolApproval`'s only
                // legal exits are `ToolExecution` (something ran) or
                // straight to `PersistTurn` (nothing did) — mirrors the
                // same branch `resolve_tool_call`'s manual path takes on a
                // denial.
                advance(machine, LoopPhase::PersistTurn)?;
                let fallback =
                    "I don't have permission to run the tool(s) needed to answer that.".to_string();
                self.events.publish(RuntimeEvent::ModelStreamChunk {
                    turn_id,
                    delta: fallback.clone(),
                });
                return Ok(TurnOutcome {
                    text: fallback,
                    usage,
                    stop_reason: StopReason::ToolUse,
                });
            }

            advance(machine, LoopPhase::ToolExecution)?;
            advance(machine, LoopPhase::PostToolReflection)?;
            // Loops back to ModelInference for the next round.
        }

        advance(machine, LoopPhase::PersistTurn)?;
        let fallback =
            "I wasn't able to finish that within the allotted tool-call steps.".to_string();
        self.events.publish(RuntimeEvent::ModelStreamChunk {
            turn_id,
            delta: fallback.clone(),
        });
        Ok(TurnOutcome {
            text: fallback,
            usage,
            stop_reason: StopReason::Other("max_tool_rounds".to_string()),
        })
    }

    /// Runs one invocation through `execute_gated`; if the policy requires
    /// a human decision, publishes `ToolApprovalRequested` (so the TUI
    /// shows the same approval modal a manual `/tool` call would) and
    /// waits for `supply_tool_decision` to unblock it — the turn's async
    /// task simply awaits, which does not block the TUI's render loop
    /// (that loop only ever polls channels/receivers, never this future).
    /// Falls back to `DeniedOnce` if the decision channel is dropped
    /// (e.g. the session ends) rather than hanging forever.
    async fn resolve_gated_call(
        &mut self,
        turn_id: TurnId,
        invocation: ToolInvocation,
        cancel: &CancellationToken,
    ) -> Result<GatedOutcome, ToolError> {
        let id = invocation.id;
        let tool_ctx = ToolContext::new(cancel.clone());
        let first_pass = execute_gated(
            &self.registry,
            self.policy.as_ref(),
            &self.approval_ctx,
            invocation.clone(),
            None,
            &tool_ctx,
        )
        .await?;
        if !matches!(first_pass, GatedOutcome::PendingApproval) {
            return Ok(first_pass);
        }

        self.events.publish(RuntimeEvent::ToolApprovalRequested {
            turn_id,
            tool_call_id: id,
        });
        let rx = self.tool_decisions.register(id);
        let decision = rx.await.unwrap_or(ApprovalDecision::DeniedOnce);

        execute_gated(
            &self.registry,
            self.policy.as_ref(),
            &self.approval_ctx,
            invocation,
            Some(decision),
            &tool_ctx,
        )
        .await
    }

    /// A cheap, independently-lockable handle for supplying decisions on
    /// tool calls this agent pauses mid-turn on — see `ToolDecisions`'s
    /// doc comment for why a caller needs to hold this *alongside* (not
    /// through) whatever lock guards the `Agent` itself.
    pub fn tool_decisions(&self) -> ToolDecisions {
        self.tool_decisions.clone()
    }

    /// Manually proposes a tool call for approval, bypassing the model
    /// entirely — the TUI's `/tool <name> <json>` demo command. A model
    /// requesting a tool itself goes through `run_tool_loop` instead (see
    /// `submit_message`'s doc comment); this exists for exercising/testing
    /// a specific tool directly regardless of what the model would choose.
    /// Emits the same `ToolCallProposed`/`ToolApprovalRequested` events
    /// `run_tool_loop` would.
    pub fn propose_tool_call(
        &mut self,
        tool_name: String,
        arguments: serde_json::Value,
        risk: RiskLevel,
    ) -> ToolCallId {
        let turn_id = TurnId::new();
        let invocation = ToolInvocation {
            id: ToolCallId::new(),
            source_turn: turn_id,
            tool_name: tool_name.clone(),
            arguments: arguments.clone(),
            risk,
            rationale: None,
        };
        let id = invocation.id;
        self.events.publish(RuntimeEvent::ToolCallProposed {
            turn_id,
            tool_call_id: id,
            tool_name,
            arguments,
            risk,
        });
        self.events.publish(RuntimeEvent::ToolApprovalRequested {
            turn_id,
            tool_call_id: id,
        });
        self.pending_tool_calls.insert(id, invocation);
        id
    }

    /// Resolves a pending tool call with a human decision, running it
    /// through the exact same `execute_gated` choke point every tool call
    /// must pass through (harness spec FR-4).
    pub async fn resolve_tool_call(
        &mut self,
        id: ToolCallId,
        decision: ApprovalDecision,
    ) -> Result<GatedOutcome, ToolError> {
        let invocation = self
            .pending_tool_calls
            .remove(&id)
            .ok_or_else(|| ToolError::Validation("no such pending tool call".to_string()))?;
        let turn_id = invocation.source_turn;

        let outcome = execute_gated(
            &self.registry,
            self.policy.as_ref(),
            &self.approval_ctx,
            invocation.clone(),
            Some(decision),
            &ToolContext::default(),
        )
        .await?;
        match &outcome {
            GatedOutcome::Executed(result) => {
                self.events.publish(RuntimeEvent::ToolExecuted {
                    turn_id,
                    tool_call_id: id,
                    tool_name: invocation.tool_name.clone(),
                    result: result.clone(),
                });
            }
            GatedOutcome::Denied => {
                self.events.publish(RuntimeEvent::ToolCallDenied {
                    turn_id,
                    tool_call_id: id,
                    tool_name: invocation.tool_name.clone(),
                    reason: "denied by approval policy".to_string(),
                });
            }
            GatedOutcome::PendingApproval => {}
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{ApprovalPolicyMode, ToolResult};
    use arbe_providers::{ModelCapabilities, ProviderStream};
    use async_trait::async_trait;
    use std::path::PathBuf;

    fn fake_capabilities(tool_calls: bool) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calls,
            vision: false,
            thinking: false,
            prompt_caching: false,
            max_context_tokens: 8_000,
        }
    }

    fn scripted(events: Vec<ProviderEvent>) -> ProviderStream {
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }

    /// Streams "hel" + "lo" as a plain answer, every time.
    struct FakeProvider;

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> &str {
            "fake"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            fake_capabilities(false)
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(scripted(vec![
                ProviderEvent::TextDelta("hel".to_string()),
                ProviderEvent::TextDelta("lo".to_string()),
                ProviderEvent::Usage(Usage {
                    input_tokens: 7,
                    output_tokens: 2,
                    ..Default::default()
                }),
                ProviderEvent::Stop(StopReason::EndTurn),
            ]))
        }
    }

    /// Requests one `echo` tool call (with some text alongside it) on its
    /// first call, then answers "final answer" — exercises the model loop's
    /// round trip without a real model. Records every request it receives.
    struct FakeToolCallingProvider {
        call_count: std::sync::atomic::AtomicU32,
        requests: Arc<std::sync::Mutex<Vec<ModelRequest>>>,
    }

    impl FakeToolCallingProvider {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
                requests: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for FakeToolCallingProvider {
        fn id(&self) -> &str {
            "fake"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            fake_capabilities(true)
        }

        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            self.requests.lock().unwrap().push(req);
            let round = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let usage = ProviderEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            });
            Ok(if round == 0 {
                scripted(vec![
                    ProviderEvent::TextDelta("Let me check.".to_string()),
                    ProviderEvent::ToolUseStart {
                        id: "call_1".to_string(),
                        name: "echo".to_string(),
                    },
                    ProviderEvent::ToolUseInputDelta {
                        id: "call_1".to_string(),
                        partial_json: "{\"x\":".to_string(),
                    },
                    ProviderEvent::ToolUseInputDelta {
                        id: "call_1".to_string(),
                        partial_json: "1}".to_string(),
                    },
                    usage,
                    ProviderEvent::Stop(StopReason::ToolUse),
                ])
            } else {
                scripted(vec![
                    ProviderEvent::TextDelta("final answer".to_string()),
                    usage,
                    ProviderEvent::Stop(StopReason::EndTurn),
                ])
            })
        }
    }

    struct EchoExecutor;

    #[async_trait]
    impl ToolExecutor for EchoExecutor {
        async fn execute(
            &self,
            invocation: ToolInvocation,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: invocation.arguments,
                is_error: false,
            })
        }
    }

    fn temp_store() -> (SessionStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("arbe-agent-test-{}", uuid::Uuid::new_v4()));
        (SessionStore::with_root(dir.clone()), dir)
    }

    // The registry only knows real providers, so tests construct the
    // Agent's pieces directly rather than through Agent::create/resume
    // (those exist to wire a real provider from config).
    fn agent_with_fake_provider(
        store: SessionStore,
        meta: SessionMeta,
        events: Arc<EventBus>,
    ) -> Agent {
        Agent {
            store,
            meta,
            provider: Box::new(FakeProvider),
            pipeline: ContextPipeline::default(),
            strategy: Box::new(TruncationStrategy),
            budget_tokens: 8_000,
            max_tool_rounds: 50,
            retry: RetryPolicy::none(),
            thinking_budget_tokens: None,
            temperature: 0.2,
            max_tokens: 100,
            registry: ToolRegistry::new(),
            policy: Box::new(StandardApprovalPolicy),
            approval_ctx: ApprovalContext::new(ApprovalPolicyMode::AlwaysPrompt, vec![], vec![]),
            hooks: HookRegistry::new(Duration::from_millis(500)),
            events,
            history: Vec::new(),
            pinned_turn_indices: Vec::new(),
            next_turn_index: 0,
            pending_tool_calls: HashMap::new(),
            tool_decisions: ToolDecisions::default(),
            last_estimated_tokens: 0,
            token_calibration: TokenCalibration::default(),
            // A directory that doesn't exist: no project instruction files,
            // so tests don't depend on this repo's own CLAUDE.md.
            project_dir: std::env::temp_dir().join(format!(
                "arbe-agent-test-no-project-{}",
                uuid::Uuid::new_v4()
            )),
        }
    }

    #[tokio::test]
    async fn submit_message_streams_persists_and_emits_events() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut rx = events.subscribe();
        let mut agent = agent_with_fake_provider(store.clone(), meta.clone(), events);

        let reply = agent.submit_message("hi there".to_string()).await.unwrap();
        assert_eq!(reply, "hello");

        let turns = store.list_turns(meta.id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].final_assistant_message().unwrap().text(), "hello");

        let mut saw_stream_chunk = false;
        let mut saw_turn_completed = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                RuntimeEvent::ModelStreamChunk { .. } => saw_stream_chunk = true,
                RuntimeEvent::TurnCompleted { .. } => saw_turn_completed = true,
                _ => {}
            }
        }
        assert!(saw_stream_chunk);
        assert!(saw_turn_completed);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn second_turn_sees_first_turns_history() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);

        agent.submit_message("first".to_string()).await.unwrap();
        agent.submit_message("second".to_string()).await.unwrap();

        assert_eq!(agent.history.len(), 4);
        assert_eq!(agent.next_turn_index, 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn project_instructions_are_reread_and_reflected_on_the_next_turn() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);

        let project_dir =
            std::env::temp_dir().join(format!("arbe-agent-project-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project_dir).unwrap();
        agent.project_dir = project_dir.clone();

        agent.submit_message("first".to_string()).await.unwrap();
        assert!(
            !agent.pipeline.system_instructions[0].contains("edited mid-session"),
            "system prompt should not mention a file that doesn't exist yet"
        );

        std::fs::write(project_dir.join("agent.md"), "edited mid-session").unwrap();

        agent.submit_message("second".to_string()).await.unwrap();
        assert!(
            agent.pipeline.system_instructions[0].contains("edited mid-session"),
            "system prompt should pick up an agent.md written after the session started"
        );

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&project_dir).ok();
    }

    #[tokio::test]
    async fn propose_then_resolve_tool_call_executes_through_the_gate() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut rx = events.subscribe();
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.register_tool("echo", Arc::new(EchoExecutor));

        let id = agent.propose_tool_call("echo".to_string(), json!({"x": 1}), RiskLevel::Low);

        let mut saw_proposed = false;
        let mut saw_requested = false;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                RuntimeEvent::ToolCallProposed { .. } => saw_proposed = true,
                RuntimeEvent::ToolApprovalRequested { .. } => saw_requested = true,
                _ => {}
            }
        }
        assert!(saw_proposed);
        assert!(saw_requested);

        let outcome = agent
            .resolve_tool_call(id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        assert!(matches!(outcome, GatedOutcome::Executed(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn resolving_an_unknown_tool_call_id_is_a_validation_error() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);

        let Err(err) = agent
            .resolve_tool_call(ToolCallId::new(), ApprovalDecision::ApprovedOnce)
            .await
        else {
            panic!("expected an error");
        };
        assert!(matches!(err, ToolError::Validation(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Rebuilds an Agent's in-memory history from `store.list_turns`, the
    /// same recovery logic `Agent::resume` uses — duplicated here (with a
    /// `FakeProvider` instead of going through `build_provider`) so this
    /// test exercises the exact code path a real crash-and-resume would,
    /// per harness spec FR-1 ("interrupted-session recovery").
    fn agent_resumed_with_fake_provider(
        store: SessionStore,
        meta: SessionMeta,
        events: Arc<EventBus>,
    ) -> Agent {
        let turns = store.list_turns(meta.id).unwrap();
        let mut history = Vec::new();
        let mut next_turn_index = 0;
        for turn in turns {
            // Same shape `run_turn` writes today: the user message and the
            // final answer (see `submit_message` on the tool trace).
            for m in [turn.user_message(), turn.final_assistant_message()]
                .into_iter()
                .flatten()
            {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m.clone(),
                });
            }
            next_turn_index = next_turn_index.max(turn.index + 1);
        }
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.history = history;
        agent.next_turn_index = next_turn_index;
        agent
    }

    #[tokio::test]
    async fn session_recovers_after_a_forced_interruption() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());

        {
            // First "process": submits one turn, then is dropped without
            // ever calling close() — simulating a crash/forced kill.
            let mut agent = agent_with_fake_provider(store.clone(), meta.clone(), events.clone());
            agent
                .submit_message("before the crash".to_string())
                .await
                .unwrap();
        }

        // Second "process": resumes the same session from disk.
        let mut resumed = agent_resumed_with_fake_provider(store.clone(), meta.clone(), events);
        assert_eq!(resumed.history.len(), 2);
        assert_eq!(resumed.next_turn_index, 1);

        resumed
            .submit_message("after recovery".to_string())
            .await
            .unwrap();

        let turns = store.list_turns(meta.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[1].index, 1);
        assert_eq!(turns[1].user_message().unwrap().text(), "after recovery");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn memory_strategy_is_swappable_via_config_alone() {
        // Same history, same tiny budget, only the strategy differs — the
        // implementation plan's Phase 3 exit criterion ("config can switch
        // strategy... without code changes") exercised at the Agent level.
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());

        let long_history = vec![
            HistoryEntry {
                turn_index: 0,
                message: Message::new(Role::User, "a".repeat(200)),
            },
            HistoryEntry {
                turn_index: 1,
                message: Message::new(Role::Assistant, "b".repeat(200)),
            },
        ];

        let mut truncation_agent =
            agent_with_fake_provider(store.clone(), meta.clone(), events.clone());
        truncation_agent.strategy = build_strategy("truncation");
        truncation_agent.budget_tokens = 5;
        truncation_agent.history = long_history.clone();
        truncation_agent
            .submit_message("go".to_string())
            .await
            .unwrap();
        let truncation_context = truncation_agent.pipeline.assemble(
            truncation_agent.strategy.as_ref(),
            &long_history,
            &[],
            Message::new(Role::User, "go"),
            5,
        );

        let compact_context = {
            let mut compact_agent = agent_with_fake_provider(store, meta, events);
            compact_agent.strategy = build_strategy("compact_summary");
            compact_agent.pipeline.assemble(
                compact_agent.strategy.as_ref(),
                &long_history,
                &[],
                Message::new(Role::User, "go"),
                5,
            )
        };

        // Truncation drops old messages silently; compact_summary leaves a
        // visible marker behind — same input, different config, visibly
        // different output.
        assert!(
            !truncation_context
                .messages
                .iter()
                .any(|m| m.text().contains("compacted"))
        );
        assert!(
            compact_context
                .messages
                .iter()
                .any(|m| m.text().contains("compacted"))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Exercises the real `Agent::create` path (not `agent_with_fake_provider`,
    /// which builds an `Agent` by hand and so never runs the builtin-tool
    /// registration in `assemble`) to prove the builtin tools registered in
    /// Phase 8 are actually reachable end to end: propose a `list_dir` call,
    /// approve it, and confirm it reports the real file on disk.
    #[tokio::test]
    async fn builtin_tools_are_registered_and_usable_through_the_real_agent() {
        let (store, store_dir) = temp_store();
        let project_dir =
            std::env::temp_dir().join(format!("arbe-agent-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("marker.txt"), "hi").unwrap();

        let config = RuntimeConfig {
            project_dir: project_dir.clone(),
            ..RuntimeConfig::from_env()
        };
        let events = Arc::new(EventBus::default());
        let mut agent = Agent::create(&config, store, events).unwrap();

        let id = agent.propose_tool_call("list_dir".to_string(), json!({}), RiskLevel::Low);
        let outcome = agent
            .resolve_tool_call(id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();

        let GatedOutcome::Executed(result) = outcome else {
            panic!("expected the tool to execute, got {outcome:?}");
        };
        let names: Vec<&str> = result.output["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["marker.txt"]);

        std::fs::remove_dir_all(&store_dir).ok();
        std::fs::remove_dir_all(&project_dir).ok();
    }

    /// A provider whose every call fails, for error-path tests.
    struct FailingProvider;

    #[async_trait]
    impl ModelProvider for FailingProvider {
        fn id(&self) -> &str {
            "failing"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            fake_capabilities(false)
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            Err(ProviderError::Auth("bad key".to_string()))
        }
    }

    #[tokio::test]
    async fn a_failed_turn_publishes_a_runtime_error_event_with_its_turn_id() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut rx = events.subscribe();
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.provider = Box::new(FailingProvider);

        let err = agent.submit_message("hi".to_string()).await.unwrap_err();
        assert!(matches!(
            err,
            HarnessError::Provider(ProviderError::Auth(_))
        ));

        let mut started = None;
        let mut errored = None;
        while let Ok(event) = rx.try_recv() {
            match event.event {
                RuntimeEvent::TurnStarted { turn_id, .. } => started = Some(turn_id),
                RuntimeEvent::RuntimeError { turn_id, reason } => {
                    assert!(reason.contains("bad key"));
                    errored = turn_id;
                }
                _ => {}
            }
        }
        assert!(started.is_some());
        assert_eq!(started, errored);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_tool_loop_auto_executes_an_allowlisted_tool_and_returns_final_content() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.provider = Box::new(FakeToolCallingProvider::new());
        agent.register_tool("echo", Arc::new(EchoExecutor));
        agent.approval_ctx = ApprovalContext::new(
            ApprovalPolicyMode::AllowlistAuto,
            vec!["echo".to_string()],
            vec![],
        );

        let reply = agent.submit_message("use echo".to_string()).await.unwrap();
        assert_eq!(reply, "final answer");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn tool_round_sends_back_the_whole_assistant_message_and_the_tool_result() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);
        let provider = FakeToolCallingProvider::new();
        let requests = provider.requests.clone();
        agent.provider = Box::new(provider);
        agent.register_tool("echo", Arc::new(EchoExecutor));
        agent.approval_ctx = ApprovalContext::new(
            ApprovalPolicyMode::AllowlistAuto,
            vec!["echo".to_string()],
            vec![],
        );

        agent.submit_message("use echo".to_string()).await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second = &requests[1].messages;
        let assistant = &second[second.len() - 2];
        assert_eq!(assistant.role, Role::Assistant);
        // Text the model wrote alongside its tool call is kept, and the
        // streamed argument fragments were reassembled.
        assert_eq!(assistant.text(), "Let me check.");
        assert_eq!(assistant.tool_uses()[0].arguments, json!({"x": 1}));
        let result = second.last().unwrap();
        assert_eq!(result.role, Role::Tool);
        let ContentBlock::ToolResult {
            tool_use_id,
            is_error,
            ..
        } = &result.content[0]
        else {
            panic!("expected a tool result, got {result:?}");
        };
        assert_eq!(tool_use_id, "call_1");
        assert!(!is_error);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_failing_tool_call_goes_back_to_the_model_instead_of_failing_the_turn() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);
        let provider = FakeToolCallingProvider::new();
        let requests = provider.requests.clone();
        agent.provider = Box::new(provider);
        // `echo` is deliberately not registered: the call fails validation.

        let reply = agent.submit_message("use echo".to_string()).await.unwrap();
        assert_eq!(reply, "final answer");

        let requests = requests.lock().unwrap();
        let result = requests[1].messages.last().unwrap();
        let ContentBlock::ToolResult {
            content, is_error, ..
        } = &result.content[0]
        else {
            panic!("expected a tool result, got {result:?}");
        };
        assert!(is_error);
        assert!(
            Message::with_blocks(Role::Tool, content.clone())
                .text()
                .contains("no tool registered")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Answers "ok" and reports a fixed, very large input token count.
    struct BigInputProvider;

    #[async_trait]
    impl ModelProvider for BigInputProvider {
        fn id(&self) -> &str {
            "big-input"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            fake_capabilities(false)
        }

        async fn stream(
            &self,
            _req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(scripted(vec![
                ProviderEvent::TextDelta("ok".to_string()),
                ProviderEvent::Usage(Usage {
                    input_tokens: 10_000_000,
                    output_tokens: 1,
                    ..Default::default()
                }),
            ]))
        }
    }

    #[tokio::test]
    async fn reported_input_tokens_calibrate_later_estimates() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.provider = Box::new(BigInputProvider);

        agent.submit_message("hi".to_string()).await.unwrap();
        // Far more real tokens than estimated: the factor moves up
        // (clamped per observation, smoothed across them).
        let factor = agent.token_calibration.factor();
        assert!(factor > 1.0, "factor {factor}");

        agent.submit_message("hi".to_string()).await.unwrap();
        // Each turn's first request is another observation.
        assert!(agent.token_calibration.factor() > factor);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn usage_is_recorded_on_the_turn_and_the_session_and_published() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut rx = events.subscribe();
        let mut agent = agent_with_fake_provider(store.clone(), meta.clone(), events);
        agent.provider = Box::new(FakeToolCallingProvider::new());
        agent.register_tool("echo", Arc::new(EchoExecutor));
        agent.approval_ctx = ApprovalContext::new(
            ApprovalPolicyMode::AllowlistAuto,
            vec!["echo".to_string()],
            vec![],
        );

        agent.submit_message("use echo".to_string()).await.unwrap();

        // Two inference rounds of 10 in / 5 out each.
        let turns = store.list_turns(meta.id).unwrap();
        assert_eq!(turns[0].usage.input_tokens, 20);
        assert_eq!(turns[0].usage.output_tokens, 10);
        assert_eq!(turns[0].stop_reason, Some(StopReason::EndTurn));
        let saved = store.load_meta(meta.id).unwrap();
        assert_eq!(saved.usage.total_tokens(), 30);

        let mut published = None;
        while let Ok(envelope) = rx.try_recv() {
            if let RuntimeEvent::UsageUpdated { session, .. } = envelope.event {
                published = Some(session);
            }
        }
        assert_eq!(published.unwrap().total_tokens(), 30);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn run_tool_loop_returns_a_fallback_message_when_every_call_is_denied() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.provider = Box::new(FakeToolCallingProvider::new());
        agent.register_tool("echo", Arc::new(EchoExecutor));
        agent.approval_ctx = ApprovalContext::new(
            ApprovalPolicyMode::DenylistBlock,
            vec![],
            vec!["echo".to_string()],
        );

        let reply = agent.submit_message("use echo".to_string()).await.unwrap();
        assert!(reply.contains("don't have permission"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The regression test for the deadlock `ToolDecisions` exists to
    /// avoid: with the default `AlwaysPrompt` policy, `run_tool_loop`
    /// pauses awaiting a decision. This drives `submit_message` on a
    /// spawned task (mirroring how the TUI runs it behind a
    /// `tokio::sync::Mutex`) and supplies the decision from the outside,
    /// via a `ToolDecisions` handle obtained *before* the task started —
    /// proving the decision path never needs to re-enter `&mut Agent`.
    #[tokio::test]
    async fn run_tool_loop_pauses_for_a_human_decision_and_resumes_via_tool_decisions() {
        let (store, dir) = temp_store();
        let meta = store
            .create_session("default", "fake", "fake-model")
            .unwrap();
        let events = Arc::new(EventBus::default());
        let mut events_rx = events.subscribe();
        let mut agent = agent_with_fake_provider(store, meta, events);
        agent.provider = Box::new(FakeToolCallingProvider::new());
        agent.register_tool("echo", Arc::new(EchoExecutor));
        // agent_with_fake_provider defaults to AlwaysPrompt.

        let decisions = agent.tool_decisions();
        let turn = tokio::spawn(async move { agent.submit_message("use echo".to_string()).await });

        let tool_call_id = loop {
            match tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
                .await
                .expect("timed out waiting for ToolApprovalRequested")
                .unwrap()
                .event
            {
                RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => break tool_call_id,
                _ => continue,
            }
        };

        assert!(decisions.supply(tool_call_id, ApprovalDecision::ApprovedOnce));

        let reply = tokio::time::timeout(Duration::from_secs(2), turn)
            .await
            .expect("submit_message never returned — likely deadlocked")
            .unwrap()
            .unwrap();
        assert_eq!(reply, "final answer");

        std::fs::remove_dir_all(&dir).ok();
    }
}
