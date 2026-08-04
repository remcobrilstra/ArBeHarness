use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arbe_core::{
    ApprovalDecision, HarnessError, LoopMachine, LoopPhase, MemoryError, Message, ProviderError,
    RiskLevel, Role, RuntimeEvent, SessionId, SessionMeta, SessionStatus, ToolCallId, ToolError,
    ToolInvocation, ToolSpec, Turn, TurnId,
};
use arbe_hooks::{HookPhase, HookRegistry};
use arbe_memory::{
    CompactWithSummaryStrategy, ContextPipeline, ContextStrategy, HistoryEntry, TruncationStrategy,
};
use arbe_providers::{ModelProvider, ModelRequest, build_provider};
use arbe_skills::SkillScope;
use arbe_storage::SessionStore;
use arbe_tools::{
    ApprovalContext, ApprovalPolicy, GatedOutcome, StandardApprovalPolicy, ToolExecutor,
    ToolRegistry, execute_gated,
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
        let provider = build_provider(
            &config.provider_name,
            config.api_key.clone(),
            config.base_url.clone(),
        )?;

        let mut registry = ToolRegistry::new();
        arbe_tools::builtin::register_all(&mut registry, &config.project_dir);

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
            budget_tokens: config.context_budget_tokens,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            registry,
            policy: Box::new(StandardApprovalPolicy),
            approval_ctx: ApprovalContext {
                policy_mode: config.policy_mode,
                allowlist: config.allowlist.clone(),
                denylist: config.denylist.clone(),
            },
            hooks: HookRegistry::new(Duration::from_millis(config.hook_timeout_ms)),
            events,
            history,
            pinned_turn_indices: Vec::new(),
            next_turn_index: 0,
            pending_tool_calls: HashMap::new(),
            tool_decisions: ToolDecisions::default(),
            last_estimated_tokens: 0,
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
            if let Some(m) = turn.user_message {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m,
                });
            }
            if let Some(m) = turn.assistant_message {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m,
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
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<RuntimeEvent> {
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

    /// Runs one full turn: assemble context, run inference (looping through
    /// approval-gated tool calls if the provider/model want any — see
    /// `run_tool_loop`), persist, emit events.
    ///
    /// Only the direct-response path (no tools requested) streams
    /// token-by-token via `ModelProvider::infer_stream`. When tools are on
    /// the table, each round instead uses the non-streaming `infer` — the
    /// model has to fully decide "call a tool" vs. "answer" before there's
    /// anything useful to show, and accumulating a *streamed* tool call
    /// (OpenAI sends its arguments as fragmented JSON-string deltas keyed
    /// by index) is real complexity this doesn't need yet. The loop's
    /// final round (the one that returns plain content, no more tool
    /// calls) still emits that content as a `ModelStreamChunk`, so from
    /// the TUI/event-consumer side both paths look the same.
    ///
    /// The intermediate tool-calling exchange (the assistant's tool-call
    /// requests and each tool's result) lives only in this turn's local
    /// `messages` — it is **not** persisted to `history`/`turns.jsonl` or
    /// replayed into a future turn's context. Only the original user
    /// message and the final assistant answer are, exactly as for a
    /// direct-response turn. That means a resumed session sees what the
    /// agent concluded, not the tool trace that got it there — an
    /// intentional v1 scope cut (persisting/replaying the full trace would
    /// mean threading `Message::tool_calls`/`tool_call_id` through
    /// `ContextPipeline`, memory strategies, and `Turn`'s schema too).
    pub async fn submit_message(&mut self, content: String) -> Result<String, HarnessError> {
        let mut machine = LoopMachine::new();
        machine
            .transition(LoopPhase::ReceiveUserInput)
            .expect("agent loop transition graph violated");
        let turn = Turn::new(self.meta.id, self.next_turn_index);
        let turn_id = turn.id;
        self.events.publish(RuntimeEvent::TurnStarted {
            session_id: self.meta.id,
            turn_id,
        });

        machine
            .transition(LoopPhase::AssembleContext)
            .expect("agent loop transition graph violated");
        self.pipeline.system_instructions =
            vec![build_system_prompt_async(&self.project_dir).await];
        let user_message = Message::new(Role::User, content);
        let context = self.pipeline.assemble(
            self.strategy.as_ref(),
            &self.history,
            &self.pinned_turn_indices,
            user_message.clone(),
            self.budget_tokens,
        );
        self.last_estimated_tokens = context.estimated_tokens;
        self.events.publish(RuntimeEvent::ContextBuilt {
            turn_id,
            estimated_tokens: context.estimated_tokens,
        });

        machine
            .transition(LoopPhase::PlanOrDirectRespond)
            .expect("agent loop transition graph violated");

        let tool_specs: Vec<ToolSpec> = if self.provider.capabilities().tool_calls {
            arbe_tools::builtin::tool_specs()
        } else {
            Vec::new()
        };

        let assistant_content = if tool_specs.is_empty() {
            machine
                .transition(LoopPhase::ModelInference)
                .expect("agent loop transition graph violated");
            self.hooks
                .run_phase(
                    HookPhase::BeforeModelCall,
                    json!({ "turn_id": turn_id.to_string(), "message_count": context.messages.len() }),
                )
                .await;

            let request = ModelRequest {
                model: self.meta.model.clone(),
                messages: context.messages,
                temperature: self.temperature,
                max_tokens: self.max_tokens,
                tools: Vec::new(),
            };
            let mut stream = self
                .provider
                .infer_stream(request)
                .await
                .map_err(HarnessError::Provider)?;

            let mut acc = String::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(HarnessError::Provider)?;
                acc.push_str(&chunk.delta);
                self.events.publish(RuntimeEvent::ModelStreamChunk {
                    turn_id,
                    delta: chunk.delta,
                });
            }

            self.hooks
                .run_phase(
                    HookPhase::AfterModelCall,
                    json!({ "turn_id": turn_id.to_string(), "response_len": acc.len() }),
                )
                .await;

            machine
                .transition(LoopPhase::InterpretOutput)
                .expect("agent loop transition graph violated");
            machine
                .transition(LoopPhase::PersistTurn)
                .expect("agent loop transition graph violated");
            acc
        } else {
            self.run_tool_loop(turn_id, context.messages, tool_specs, &mut machine)
                .await?
        };

        let assistant_message = Message::new(Role::Assistant, assistant_content.clone());
        let mut persisted_turn = turn;
        persisted_turn.user_message = Some(user_message.clone());
        persisted_turn.assistant_message = Some(assistant_message.clone());
        self.store
            .append_turn(&persisted_turn)
            .map_err(|e| HarnessError::Memory(MemoryError::StoreUnavailable(e.to_string())))?;

        self.history.push(HistoryEntry {
            turn_index: persisted_turn.index,
            message: user_message,
        });
        self.history.push(HistoryEntry {
            turn_index: persisted_turn.index,
            message: assistant_message,
        });
        self.next_turn_index += 1;

        machine
            .transition(LoopPhase::EmitEvents)
            .expect("agent loop transition graph violated");
        self.events.publish(RuntimeEvent::TurnCompleted {
            session_id: self.meta.id,
            turn_id,
        });
        machine
            .transition(LoopPhase::Idle)
            .expect("agent loop transition graph violated");

        self.hooks
            .run_phase(
                HookPhase::OnTurnComplete,
                json!({ "turn_id": turn_id.to_string() }),
            )
            .await;

        Ok(assistant_content)
    }

    /// Caps how many model<->tool round trips one turn can take before
    /// giving up and returning whatever's been learned so far as a plain
    /// message — a runaway "call a tool, get a result, call another tool"
    /// loop shouldn't be able to hang a turn forever.
    const MAX_TOOL_ROUNDS: u32 = 8;

    /// Drives the model<->tool round-trip loop for a turn whose provider
    /// supports tool calling (see `submit_message`'s doc comment for why
    /// this uses non-streaming `infer` for the decision rounds). Every
    /// call still passes through `execute_gated` — the same choke point
    /// the manual `/tool` demo path (`propose_tool_call`/`resolve_tool_call`)
    /// uses — so a model-initiated call is never less gated than a
    /// human-initiated one.
    async fn run_tool_loop(
        &mut self,
        turn_id: TurnId,
        mut messages: Vec<Message>,
        tool_specs: Vec<ToolSpec>,
        machine: &mut LoopMachine,
    ) -> Result<String, HarnessError> {
        for round in 0..Self::MAX_TOOL_ROUNDS {
            machine
                .transition(LoopPhase::ModelInference)
                .expect("agent loop transition graph violated");
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
            };
            let response = self
                .provider
                .infer(request)
                .await
                .map_err(HarnessError::Provider)?;

            self.hooks
                .run_phase(
                    HookPhase::AfterModelCall,
                    json!({ "turn_id": turn_id.to_string(), "response_len": response.content.len(), "tool_calls": response.tool_calls.len() }),
                )
                .await;

            machine
                .transition(LoopPhase::InterpretOutput)
                .expect("agent loop transition graph violated");

            if response.tool_calls.is_empty() {
                machine
                    .transition(LoopPhase::PersistTurn)
                    .expect("agent loop transition graph violated");
                // Mirrors the direct-response path's event, so a consumer
                // (the TUI transcript) doesn't need to know which branch
                // produced the final content.
                self.events.publish(RuntimeEvent::ModelStreamChunk {
                    turn_id,
                    delta: response.content.clone(),
                });
                return Ok(response.content);
            }

            machine
                .transition(LoopPhase::ToolApproval)
                .expect("agent loop transition graph violated");
            messages.push(Message::assistant_tool_calls(response.tool_calls.clone()));

            let mut any_executed = false;
            for call in response.tool_calls {
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

                let outcome = self.resolve_gated_call(turn_id, invocation).await?;

                let result_text = match &outcome {
                    GatedOutcome::Executed(result) => {
                        any_executed = true;
                        self.events.publish(RuntimeEvent::ToolExecuted {
                            turn_id,
                            tool_call_id: invocation_id,
                            tool_name: call.name.clone(),
                            result: result.clone(),
                        });
                        serde_json::to_string(&result.output).unwrap_or_default()
                    }
                    GatedOutcome::Denied => {
                        let reason = "denied by approval policy".to_string();
                        self.events.publish(RuntimeEvent::ToolCallDenied {
                            turn_id,
                            tool_call_id: invocation_id,
                            tool_name: call.name.clone(),
                            reason: reason.clone(),
                        });
                        reason
                    }
                    // `resolve_gated_call` never returns this — it always
                    // resolves a `PendingApproval` before returning.
                    GatedOutcome::PendingApproval => "still awaiting approval".to_string(),
                };
                messages.push(Message::tool_result(call.id, result_text));
            }

            if !any_executed {
                // Every call this round was denied. `ToolApproval`'s only
                // legal exits are `ToolExecution` (something ran) or
                // straight to `PersistTurn` (nothing did) — mirrors the
                // same branch `resolve_tool_call`'s manual path takes on a
                // denial.
                machine
                    .transition(LoopPhase::PersistTurn)
                    .expect("agent loop transition graph violated");
                let fallback =
                    "I don't have permission to run the tool(s) needed to answer that.".to_string();
                self.events.publish(RuntimeEvent::ModelStreamChunk {
                    turn_id,
                    delta: fallback.clone(),
                });
                return Ok(fallback);
            }

            machine
                .transition(LoopPhase::ToolExecution)
                .expect("agent loop transition graph violated");
            machine
                .transition(LoopPhase::PostToolReflection)
                .expect("agent loop transition graph violated");
            // Loops back to ModelInference for the next round.
        }

        machine
            .transition(LoopPhase::PersistTurn)
            .expect("agent loop transition graph violated");
        let fallback =
            "I wasn't able to finish that within the allotted tool-call steps.".to_string();
        self.events.publish(RuntimeEvent::ModelStreamChunk {
            turn_id,
            delta: fallback.clone(),
        });
        Ok(fallback)
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
    ) -> Result<GatedOutcome, HarnessError> {
        let id = invocation.id;
        let first_pass = execute_gated(
            &self.registry,
            self.policy.as_ref(),
            &self.approval_ctx,
            invocation.clone(),
            None,
        )
        .await
        .map_err(HarnessError::Tool)?;
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
        )
        .await
        .map_err(HarnessError::Tool)
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
    use arbe_providers::{ModelResponse, ProviderCapabilities, TokenChunk};
    use async_trait::async_trait;
    use futures_core::Stream;
    use std::path::PathBuf;

    struct FakeProvider;

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                streaming: true,
                tool_calls: false,
                json_mode: false,
                max_context_tokens: 8_000,
            }
        }

        async fn infer(&self, _req: ModelRequest) -> Result<ModelResponse, ProviderError> {
            Ok(ModelResponse {
                content: "hi".to_string(),
                tool_calls: Vec::new(),
            })
        }

        async fn infer_stream(
            &self,
            _req: ModelRequest,
        ) -> Result<
            Box<dyn Stream<Item = Result<TokenChunk, ProviderError>> + Send + Unpin>,
            ProviderError,
        > {
            let chunks = vec![
                Ok(TokenChunk {
                    delta: "hel".to_string(),
                    is_final: false,
                }),
                Ok(TokenChunk {
                    delta: "lo".to_string(),
                    is_final: true,
                }),
            ];
            Ok(Box::new(Box::pin(futures_util::stream::iter(chunks))))
        }
    }

    /// A provider that requests one `echo` tool call on its first `infer`
    /// call, then returns plain final content on the next — exercises
    /// `run_tool_loop`'s round-trip without a real model.
    struct FakeToolCallingProvider {
        call_count: std::sync::atomic::AtomicU32,
    }

    impl FakeToolCallingProvider {
        fn new() -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for FakeToolCallingProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                streaming: true,
                tool_calls: true,
                json_mode: false,
                max_context_tokens: 8_000,
            }
        }

        async fn infer(&self, _req: ModelRequest) -> Result<ModelResponse, ProviderError> {
            let round = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if round == 0 {
                Ok(ModelResponse {
                    content: String::new(),
                    tool_calls: vec![arbe_core::RequestedToolCall {
                        id: "call_1".to_string(),
                        name: "echo".to_string(),
                        arguments: json!({"x": 1}),
                    }],
                })
            } else {
                Ok(ModelResponse {
                    content: "final answer".to_string(),
                    tool_calls: Vec::new(),
                })
            }
        }

        async fn infer_stream(
            &self,
            _req: ModelRequest,
        ) -> Result<
            Box<dyn Stream<Item = Result<TokenChunk, ProviderError>> + Send + Unpin>,
            ProviderError,
        > {
            unimplemented!("run_tool_loop never streams — see submit_message's doc comment")
        }
    }

    struct EchoExecutor;

    #[async_trait]
    impl ToolExecutor for EchoExecutor {
        async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
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

    // build_provider() only knows "openai"/"ollama", so tests construct the
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
            temperature: 0.2,
            max_tokens: 100,
            registry: ToolRegistry::new(),
            policy: Box::new(StandardApprovalPolicy),
            approval_ctx: ApprovalContext {
                policy_mode: ApprovalPolicyMode::AlwaysPrompt,
                allowlist: vec![],
                denylist: vec![],
            },
            hooks: HookRegistry::new(Duration::from_millis(500)),
            events,
            history: Vec::new(),
            pinned_turn_indices: Vec::new(),
            next_turn_index: 0,
            pending_tool_calls: HashMap::new(),
            tool_decisions: ToolDecisions::default(),
            last_estimated_tokens: 0,
            project_dir: std::path::PathBuf::from("."),
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
        assert_eq!(
            turns[0].assistant_message.as_ref().unwrap().content,
            "hello"
        );

        let mut saw_stream_chunk = false;
        let mut saw_turn_completed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
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
            match event {
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
            if let Some(m) = turn.user_message {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m,
                });
            }
            if let Some(m) = turn.assistant_message {
                history.push(HistoryEntry {
                    turn_index: turn.index,
                    message: m,
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
        assert_eq!(
            turns[1].user_message.as_ref().unwrap().content,
            "after recovery"
        );

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
                .any(|m| m.content.contains("compacted"))
        );
        assert!(
            compact_context
                .messages
                .iter()
                .any(|m| m.content.contains("compacted"))
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
        agent.approval_ctx = ApprovalContext {
            policy_mode: ApprovalPolicyMode::AllowlistAuto,
            allowlist: vec!["echo".to_string()],
            denylist: vec![],
        };

        let reply = agent.submit_message("use echo".to_string()).await.unwrap();
        assert_eq!(reply, "final answer");

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
        agent.approval_ctx = ApprovalContext {
            policy_mode: ApprovalPolicyMode::DenylistBlock,
            allowlist: vec![],
            denylist: vec!["echo".to_string()],
        };

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
