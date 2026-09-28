//! The agent: one session's state plus the loop that runs its turns.
//!
//! Every public method takes `&self`. Callers share an `Agent` behind a
//! plain `Arc` — no outer lock — which is what makes it possible to cancel
//! a running turn ([`Agent::cancel_turn`]) or answer an approval prompt
//! ([`Agent::supply_tool_decision`]) *while* [`Agent::submit_message`] is
//! still in flight. Turns themselves are serialized internally: a second
//! `submit_message` during a running turn fails with `HarnessError::Busy`.
//!
//! Layout: this module owns session state and the public API; `turn` runs
//! one turn (model loop, guards, trace persistence); `tools` runs one
//! round of tool calls (approval, parallel execution); `hooks` defines the
//! hook payloads; `approvals` is the decision mailbox.

mod approvals;
mod hooks;
mod tools;
mod turn;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use arbe_core::{
    ApprovalDecision, EventEnvelope, HarnessError, Message, ProviderError, RequestedToolCall,
    RuntimeEvent, SessionId, SessionMeta, SessionStatus, StopReason, ToolCallId, Turn, TurnId,
    Usage,
};
use arbe_hooks::HookRegistry;
use arbe_memory::{
    CompactWithSummaryStrategy, ContextPipeline, ContextStrategy, HistoryEntry, TokenCalibration,
    TruncationStrategy,
};
use arbe_providers::{
    CancellationToken, ModelProvider, ProviderRegistry, ProviderSettings, RetryPolicy,
};
use arbe_skills::SkillScope;
use arbe_storage::SessionStore;
use arbe_tools::{
    ApprovalContext, ApprovalPolicy, StandardApprovalPolicy, ToolExecutor, ToolRegistry,
};

use crate::EventBus;
use crate::config::RuntimeConfig;
use approvals::ToolDecisions;

/// Fixed-for-the-session knobs, copied out of `RuntimeConfig`.
#[derive(Debug, Clone)]
struct Settings {
    session_id: SessionId,
    profile: String,
    provider_name: String,
    model: String,
    temperature: f32,
    max_tokens: u64,
    budget_tokens: u64,
    max_tool_rounds: u32,
    max_turn_tokens: Option<u64>,
    max_tool_output_chars: usize,
    retry: RetryPolicy,
    thinking_budget_tokens: Option<u64>,
    /// The repo/project this agent works on (`RuntimeConfig::project_dir`)
    /// — the sandbox root the builtin tools are registered against.
    project_dir: PathBuf,
}

/// Mutable session state. Only ever locked briefly, never across an
/// `.await`.
struct SessionState {
    meta: SessionMeta,
    history: Vec<HistoryEntry>,
    pinned_turn_indices: Vec<u64>,
    next_turn_index: u64,
    last_estimated_tokens: u64,
    calibration: TokenCalibration,
    pipeline: ContextPipeline,
}

pub struct Agent {
    settings: Settings,
    store: SessionStore,
    provider: Box<dyn ModelProvider>,
    strategy: Box<dyn ContextStrategy>,
    policy: Box<dyn ApprovalPolicy>,
    approval_ctx: ApprovalContext,
    hooks: HookRegistry,
    events: Arc<EventBus>,
    /// Copy-on-write so a running turn works from a stable snapshot while
    /// `register_tool` can still add tools.
    registry: RwLock<Arc<ToolRegistry>>,
    decisions: ToolDecisions,
    /// Held for the duration of a turn (or a manual tool call); `try_lock`
    /// failing is what makes a concurrent submission `Busy`.
    turn_lock: tokio::sync::Mutex<()>,
    /// The running turn's cancellation token, if a turn is running.
    active_cancel: Mutex<Option<CancellationToken>>,
    state: Mutex<SessionState>,
}

/// Everything needed to build an `Agent`, however it was obtained (config
/// + disk in production, fakes in tests).
struct Parts {
    settings: Settings,
    store: SessionStore,
    meta: SessionMeta,
    provider: Box<dyn ModelProvider>,
    strategy: Box<dyn ContextStrategy>,
    approval_ctx: ApprovalContext,
    hooks: HookRegistry,
    events: Arc<EventBus>,
    registry: ToolRegistry,
    history: Vec<HistoryEntry>,
    next_turn_index: u64,
    skill_instructions: Vec<String>,
}

fn build_strategy(name: &str) -> Box<dyn ContextStrategy> {
    match name {
        "compact_summary" => Box::new(CompactWithSummaryStrategy),
        _ => Box::new(TruncationStrategy),
    }
}

/// Reads `<arbe_home>/instructions/agent.md` and
/// `<project_dir>/agent.md`/`CLAUDE.md`, then renders them into the system
/// prompt template (`crate::system_prompt`). Re-read on every turn rather
/// than cached, so edits take effect on the next turn without restarting.
/// A read error degrades to an absent section — instructions are additive,
/// not load-bearing, so a transient error can't take down a turn.
fn build_system_prompt(project_dir: &Path) -> String {
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

/// [`build_system_prompt`] off the async executor: `arbe_storage`'s readers
/// are synchronous `std::fs` calls.
async fn build_system_prompt_async(project_dir: &Path) -> String {
    let project_dir = project_dir.to_path_buf();
    tokio::task::spawn_blocking(move || build_system_prompt(&project_dir))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "system prompt render task panicked; using template with no instructions");
            crate::system_prompt::render_system_prompt(None, None)
        })
}

/// Loads global skills from `~/.arbe/skills/` (harness spec FR-6). A
/// missing/unreadable directory degrades to no skills.
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

/// Every message of every persisted turn, tagged with its turn index — the
/// in-memory history a resumed session continues from.
fn history_from_turns(turns: &[Turn]) -> (Vec<HistoryEntry>, u64) {
    let mut history = Vec::new();
    let mut next_turn_index = 0;
    for turn in turns {
        history.extend(turn.messages.iter().map(|message| HistoryEntry {
            turn_index: turn.index,
            message: message.clone(),
        }));
        next_turn_index = next_turn_index.max(turn.index + 1);
    }
    (history, next_turn_index)
}

/// If the previous process died mid-turn, its messages are still in the
/// in-flight log: turn them into a proper `Turn` record (stop reason
/// `Interrupted`, unanswered tool calls closed with error results) so the
/// history stays valid, then clear the log.
fn recover_interrupted_turn(
    store: &SessionStore,
    session_id: SessionId,
) -> Result<(), arbe_storage::StorageError> {
    let in_flight = store.read_in_flight(session_id)?;
    let Some(first) = in_flight.first() else {
        return Ok(());
    };
    let already_committed = store
        .list_turns(session_id)?
        .iter()
        .any(|t| t.id == first.turn_id);
    if !already_committed {
        let mut turn = Turn::new(session_id, first.turn_index);
        turn.id = first.turn_id;
        turn.messages = in_flight
            .iter()
            .filter(|e| e.turn_id == first.turn_id)
            .map(|e| e.message.clone())
            .collect();
        turn::close_dangling_tool_uses(&mut turn.messages);
        turn.stop_reason = Some(StopReason::Interrupted);
        store.append_turn(&turn)?;
        tracing::info!(turn_index = turn.index, "recovered an interrupted turn");
    }
    store.clear_in_flight(session_id)
}

impl Agent {
    fn from_parts(parts: Parts) -> Self {
        Self {
            settings: parts.settings,
            store: parts.store,
            provider: parts.provider,
            strategy: parts.strategy,
            policy: Box::new(StandardApprovalPolicy),
            approval_ctx: parts.approval_ctx,
            hooks: parts.hooks,
            events: parts.events,
            registry: RwLock::new(Arc::new(parts.registry)),
            decisions: ToolDecisions::default(),
            turn_lock: tokio::sync::Mutex::new(()),
            active_cancel: Mutex::new(None),
            state: Mutex::new(SessionState {
                meta: parts.meta,
                history: parts.history,
                pinned_turn_indices: Vec::new(),
                next_turn_index: parts.next_turn_index,
                last_estimated_tokens: 0,
                calibration: TokenCalibration::default(),
                pipeline: ContextPipeline {
                    skill_instructions: parts.skill_instructions,
                    ..Default::default()
                },
            }),
        }
    }

    fn assemble(
        config: &RuntimeConfig,
        store: SessionStore,
        meta: SessionMeta,
        events: Arc<EventBus>,
        history: Vec<HistoryEntry>,
        next_turn_index: u64,
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

        Ok(Self::from_parts(Parts {
            settings: Settings {
                session_id: meta.id,
                profile: meta.profile.clone(),
                provider_name: meta.provider.clone(),
                model: meta.model.clone(),
                temperature: config.temperature,
                max_tokens: config.max_tokens,
                budget_tokens,
                max_tool_rounds: config.max_tool_rounds,
                max_turn_tokens: config.max_turn_tokens,
                max_tool_output_chars: config.max_tool_output_chars,
                retry: config.retry,
                thinking_budget_tokens: config.thinking_budget_tokens,
                project_dir: config.project_dir.clone(),
            },
            store,
            meta,
            provider,
            strategy: build_strategy(&config.memory_strategy),
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
            registry,
            history,
            next_turn_index,
            skill_instructions: load_global_skill_instructions(),
        }))
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
        Self::assemble(config, store, meta, events, Vec::new(), 0)
    }

    /// Resumes a session from disk (harness spec FR-1): recovers a turn the
    /// previous process didn't finish, then rebuilds history from every
    /// persisted turn's full message trace.
    pub fn resume(
        config: &RuntimeConfig,
        store: SessionStore,
        session_id: SessionId,
        events: Arc<EventBus>,
    ) -> Result<Self, ProviderError> {
        let meta = store
            .resume_session(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to resume session: {e}")))?;
        recover_interrupted_turn(&store, session_id).map_err(|e| {
            ProviderError::Internal(format!("failed to recover interrupted turn: {e}"))
        })?;
        let turns = store
            .list_turns(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to load session history: {e}")))?;
        let (history, next_turn_index) = history_from_turns(&turns);

        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        Self::assemble(config, store, meta, events, history, next_turn_index)
    }

    fn state(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn registry_snapshot(&self) -> Arc<ToolRegistry> {
        self.registry
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn session_id(&self) -> SessionId {
        self.settings.session_id
    }

    pub fn profile(&self) -> &str {
        &self.settings.profile
    }

    pub fn provider_name(&self) -> &str {
        &self.settings.provider_name
    }

    pub fn model(&self) -> &str {
        &self.settings.model
    }

    /// The repo/project directory this agent works on.
    pub fn project_dir(&self) -> &Path {
        &self.settings.project_dir
    }

    /// Lets any client observe this agent's event stream without coupling
    /// to loop internals (TUI spec §5).
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<EventEnvelope> {
        self.events.subscribe()
    }

    /// Calibrated context-size estimate from the most recent turn, for a
    /// status display (TUI-FR-4); `0` before the first turn.
    pub fn last_estimated_tokens(&self) -> u64 {
        self.state().last_estimated_tokens
    }

    /// Provider-reported token usage over the whole session.
    pub fn usage(&self) -> Usage {
        self.state().meta.usage
    }

    /// Whether a turn (or manual tool call) is running.
    pub fn is_busy(&self) -> bool {
        self.turn_lock.try_lock().is_err()
    }

    /// Adds a tool for the model (and `invoke_tool`) to call. Takes effect
    /// from the next tool round.
    pub fn register_tool(&self, name: impl Into<String>, executor: Arc<dyn ToolExecutor>) {
        let mut registry = self.registry.write().unwrap_or_else(|p| p.into_inner());
        Arc::make_mut(&mut registry).register(name, executor);
    }

    /// Marks the session closed. Doesn't interrupt a running turn — call
    /// [`cancel_turn`](Self::cancel_turn) first for that.
    pub fn close(&self) -> Result<(), arbe_storage::StorageError> {
        let mut state = self.state();
        state.meta.touch(SessionStatus::Closed);
        self.store.save_meta(&state.meta)
    }

    /// Cancels the running turn, if any: streaming stops, running tools are
    /// told to stop (`execute` kills its process tree), pending approvals
    /// resolve as denied. What the turn produced so far is kept. Returns
    /// whether there was a turn to cancel.
    pub fn cancel_turn(&self) -> bool {
        match &*self.active_cancel.lock().unwrap_or_else(|p| p.into_inner()) {
            Some(cancel) => {
                cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Answers a `ToolApprovalRequested` prompt. Returns `false` if nothing
    /// is waiting on `id` (already answered, timed out, or cancelled).
    pub fn supply_tool_decision(&self, id: ToolCallId, decision: ApprovalDecision) -> bool {
        self.decisions.supply(id, decision)
    }

    /// Takes the turn lock and installs a fresh cancellation token for the
    /// duration of the returned guard.
    fn begin_exclusive(&self) -> Result<ActiveTurn<'_>, HarnessError> {
        let lock = self.turn_lock.try_lock().map_err(|_| HarnessError::Busy)?;
        let cancel = CancellationToken::new();
        *self.active_cancel.lock().unwrap_or_else(|p| p.into_inner()) = Some(cancel.clone());
        Ok(ActiveTurn {
            agent: self,
            cancel,
            _lock: lock,
        })
    }

    /// Runs one full turn — context assembly, then model rounds with
    /// approval-gated tool calls in between, until the model answers or a
    /// loop guard stops it — and returns the final answer's text.
    ///
    /// Every message of the turn (tool calls and results included) is
    /// persisted as it's produced and replayed into later turns' context.
    /// Errors are also published as `RuntimeEvent::RuntimeError`;
    /// cancellation as `TurnCancelled`.
    pub async fn submit_message(&self, content: String) -> Result<String, HarnessError> {
        let active = self.begin_exclusive()?;
        turn::run_turn(self, content, &active.cancel).await
    }

    /// Runs one tool directly, as if the model had requested it — the TUI's
    /// `/tool <name> <json>` command. Goes through exactly the same path as
    /// a model-initiated call: hooks, the approval gate (prompting via
    /// `ToolApprovalRequested` when policy requires), events. Returns the
    /// tool-result message.
    pub async fn invoke_tool(
        &self,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Result<Message, HarnessError> {
        let active = self.begin_exclusive()?;
        let call = RequestedToolCall {
            id: "manual".to_string(),
            name: name.into(),
            arguments,
        };
        let outcome = tools::run_round(self, TurnId::new(), vec![call], &active.cancel).await;
        if outcome.cancelled {
            return Err(HarnessError::Cancelled);
        }
        Ok(outcome.message)
    }
}

/// Clears the active cancellation token when a turn ends, however it ends.
struct ActiveTurn<'a> {
    agent: &'a Agent,
    cancel: CancellationToken,
    _lock: tokio::sync::MutexGuard<'a, ()>,
}

impl Drop for ActiveTurn<'_> {
    fn drop(&mut self) {
        *self
            .agent
            .active_cancel
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
    }
}
