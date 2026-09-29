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
mod compaction;
mod hooks;
mod memory;
mod nested;
mod skills;
mod subagent;
mod tools;
mod turn;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::Duration;

use arbe_core::{
    ApprovalDecision, Compaction, EventEnvelope, HarnessError, Message, ProviderError,
    RequestedToolCall, RuntimeEvent, SessionActivity, SessionId, SessionMeta, SessionStatus,
    StopReason, ToolCallId, Turn, TurnId, Usage,
};
use arbe_hooks::HookRegistry;
use arbe_memory::{
    CompactWithSummaryStrategy, ContextPipeline, ContextStrategy, HistoryEntry, TokenCalibration,
    TruncationStrategy,
};
use arbe_providers::{
    CancellationToken, ModelProvider, ProviderRegistry, ProviderSettings, RetryPolicy,
};
use arbe_storage::SessionStore;
use arbe_tools::{
    ApprovalContext, ApprovalPolicy, StandardApprovalPolicy, ToolExecutor, ToolRegistry,
};

use crate::EventBus;
use crate::config::{RuntimeConfig, SkillsMode, tool_allowed};
use crate::redact::Redactor;
use crate::system_prompt::PromptTemplate;
use approvals::ToolDecisions;
use arbe_mcp::{McpManager, ServerStatus, ToolSink};

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
    /// Summarize old turns with the model when history grows too large
    /// (`memory_strategy = "compact_summary"`).
    auto_compact: bool,
    retry: RetryPolicy,
    thinking_budget_tokens: Option<u64>,
    /// The repo/project this agent works on (`RuntimeConfig::project_dir`)
    /// — the sandbox root the builtin tools are registered against.
    project_dir: PathBuf,
    /// The profile's system prompt template, rendered every turn.
    prompt: PromptTemplate,
    /// Harness home (`~/.arbe`): global instructions and persistent memory.
    home: PathBuf,
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
    /// The latest compaction; history holds only the turns after it.
    summary: Option<Compaction>,
    /// Subdirectories whose nested instruction files were already shown.
    shown_instruction_dirs: std::collections::HashSet<PathBuf>,
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
    /// `register_tool` (and MCP servers connecting in the background) can
    /// still change it. Shared with [`RegistrySink`].
    registry: Arc<RwLock<Arc<ToolRegistry>>>,
    /// The profile's tool allow-set, applied to tools added later too.
    allowed_tools: Option<Vec<String>>,
    /// This session's MCP servers, if any are configured.
    mcp: Option<Arc<McpManager>>,
    /// Shared by a whole tree of subagents: answering here answers any of
    /// them.
    decisions: Arc<ToolDecisions>,
    /// Held for the duration of a turn (or a manual tool call); `try_lock`
    /// failing is what makes a concurrent submission `Busy`.
    turn_lock: tokio::sync::Mutex<()>,
    /// The running turn's cancellation token, if a turn is running.
    active_cancel: Mutex<Option<CancellationToken>>,
    state: Mutex<SessionState>,
    /// Problems found while setting up the session that the user should
    /// hear about (e.g. skill files that couldn't be loaded).
    startup_warnings: Vec<String>,
    /// Scrubs known secrets from tool output before it's used anywhere.
    redactor: Redactor,
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
    summary: Option<Compaction>,
    skill_instructions: Vec<String>,
    allowed_tools: Option<Vec<String>>,
    startup_warnings: Vec<String>,
    redactor: Redactor,
    decisions: Arc<ToolDecisions>,
}

/// Records in `meta.json` where the session works, which profile, provider
/// and model it now runs on (a resumed session takes the current
/// configuration's — that's how a conversation moves to another model),
/// and that this process has it open (idle), so other programs can find
/// and track it.
fn mark_open(meta: &mut SessionMeta, config: &RuntimeConfig, store: &SessionStore) {
    meta.profile = config.profile.clone();
    meta.provider = config.provider_name.clone();
    meta.model = config.model.clone();
    let workdir =
        std::path::absolute(&config.project_dir).unwrap_or_else(|_| config.project_dir.clone());
    meta.branch = crate::git::current_branch(&workdir);
    meta.workdir = Some(workdir);
    meta.activity = Some(SessionActivity::Idle);
    meta.pid = Some(std::process::id());
    if let Err(err) = store.save_meta(meta) {
        tracing::warn!(%err, "failed to update session metadata");
    }
}

/// A session title from its first message: the first line, shortened.
fn title_from(message: &str) -> Option<String> {
    const MAX_CHARS: usize = 60;
    let line = message.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(if line.chars().count() > MAX_CHARS {
        let cut: String = line.chars().take(MAX_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        line.to_string()
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
/// prompt template (`crate::system_prompt`). Re-read on every turn rather
/// than cached, so edits take effect on the next turn without restarting.
/// A read error degrades to an absent section — instructions are additive,
/// not load-bearing, so a transient error can't take down a turn.
fn build_system_prompt(template: &PromptTemplate, project_dir: &Path, home: &Path) -> String {
    let global =
        arbe_storage::instructions::read_global_instructions_at(home).unwrap_or_else(|err| {
            tracing::warn!(%err, "failed to read global instructions; continuing without them");
            None
        });
    let project = arbe_storage::instructions::read_project_instructions(project_dir)
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "failed to read project instructions; continuing without them");
            None
        });
    crate::system_prompt::render_template(&template.text(), global.as_deref(), project.as_deref())
}

/// [`build_system_prompt`] off the async executor: `arbe_storage`'s readers
/// (and a custom template file) are synchronous `std::fs` reads.
async fn build_system_prompt_async(
    template: &PromptTemplate,
    project_dir: &Path,
    home: &Path,
) -> String {
    let template = template.clone();
    let project_dir = project_dir.to_path_buf();
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || build_system_prompt(&template, &project_dir, &home))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "system prompt render task panicked; using template with no instructions");
            crate::system_prompt::render_system_prompt(None, None)
        })
}

/// Everything this session knows to be secret: the provider key, MCP
/// bearer tokens, and header values whose *name* says they're credentials
/// (`Authorization`, `X-Api-Key`, ...), plus secret-looking environment
/// variables (see `Redactor`). Other configured values (URLs, plain
/// headers, MCP `env` entries) are left alone: redacting them would hide
/// ordinary output for no benefit, and any secret among a stdio server's
/// variables is caught by the environment-name rule anyway.
fn redactor_for(config: &RuntimeConfig) -> Redactor {
    fn credential_headers<'a>(
        headers: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> impl Iterator<Item = String> {
        headers.into_iter().filter_map(|(name, value)| {
            let name = name.to_ascii_lowercase();
            ["auth", "key", "token", "secret", "cookie"]
                .iter()
                .any(|marker| name.contains(marker))
                .then(|| value.clone())
        })
    }
    let mut explicit: Vec<String> = config.api_key.clone().into_iter().collect();
    explicit.extend(credential_headers(
        config.extra_headers.iter().map(|(k, v)| (k, v)),
    ));
    for server in &config.mcp_servers {
        if let arbe_mcp::TransportConfig::Http {
            headers,
            bearer_token,
            ..
        } = &server.transport
        {
            explicit.extend(credential_headers(headers));
            explicit.extend(bearer_token.clone());
        }
    }
    Redactor::from_process_env(explicit)
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
            registry: Arc::new(RwLock::new(Arc::new(parts.registry))),
            allowed_tools: parts.allowed_tools,
            mcp: None,
            startup_warnings: parts.startup_warnings,
            redactor: parts.redactor,
            decisions: parts.decisions,
            turn_lock: tokio::sync::Mutex::new(()),
            active_cancel: Mutex::new(None),
            state: Mutex::new(SessionState {
                meta: parts.meta,
                history: parts.history,
                pinned_turn_indices: Vec::new(),
                next_turn_index: parts.next_turn_index,
                last_estimated_tokens: 0,
                calibration: TokenCalibration::default(),
                summary: parts.summary,
                shown_instruction_dirs: Default::default(),
                pipeline: ContextPipeline {
                    skill_instructions: parts.skill_instructions,
                    ..Default::default()
                },
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        config: &RuntimeConfig,
        providers: &ProviderRegistry,
        store: SessionStore,
        meta: SessionMeta,
        events: Arc<EventBus>,
        history: Vec<HistoryEntry>,
        next_turn_index: u64,
        summary: Option<Compaction>,
        lineage: subagent::Lineage,
    ) -> Result<Self, ProviderError> {
        let provider = providers.build(
            &config.provider_name,
            ProviderSettings {
                api_key: config.api_key.clone(),
                base_url: config.base_url.clone(),
                extra_headers: config.extra_headers.clone(),
                catalog: config.catalog.clone(),
            },
        )?;
        let mut registry = ToolRegistry::new();
        arbe_tools::builtin::register_all(&mut registry, &config.project_dir);
        registry.register(
            memory::REMEMBER_TOOL,
            Arc::new(memory::RememberTool::new(
                config.home.clone(),
                config.project_dir.clone(),
            )),
        );
        // Registered before the allow-set is applied, so a profile decides
        // whether its agent may start subagents.
        if lineage.depth < config.subagent_max_depth {
            registry.register(
                subagent::TASK_TOOL,
                Arc::new(subagent::TaskTool::new(
                    config,
                    providers,
                    store.clone(),
                    events.clone(),
                    &lineage,
                    meta.id,
                )),
            );
        }
        if let Some(allowed) = &config.tools {
            registry.retain(|name| tool_allowed(allowed, name));
        }
        let capabilities = provider.capabilities(&config.model);
        let budget_tokens = config.effective_context_budget(capabilities.max_context_tokens);

        let (skills, skill_problems) =
            skills::load_session_skills(&config.home.join("skills"), &config.project_dir);
        let startup_warnings: Vec<String> = config
            .warnings
            .iter()
            .cloned()
            .chain(skill_problems)
            .collect();
        for warning in &startup_warnings {
            tracing::warn!("{warning}");
        }
        // On demand needs tool calling; otherwise every skill goes in the
        // prompt. The loader is registered after the allow-set is applied:
        // it's part of how skills work, not a tool a profile opts into.
        let skill_instructions = if skills.is_empty() {
            Vec::new()
        } else if config.skills_mode == SkillsMode::OnDemand && capabilities.tool_calls {
            let index = skills.index(skills::LOAD_SKILL_TOOL);
            registry.register(
                skills::LOAD_SKILL_TOOL,
                Arc::new(skills::LoadSkillTool::new(skills)),
            );
            vec![index]
        } else {
            skills.instructions()
        };

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
                auto_compact: config.memory_strategy == "compact_summary",
                retry: config.retry,
                thinking_budget_tokens: config.thinking_budget_tokens,
                project_dir: config.project_dir.clone(),
                prompt: config.prompt.clone(),
                home: config.home.clone(),
            },
            store,
            meta,
            provider,
            strategy: build_strategy(&config.memory_strategy),
            approval_ctx: ApprovalContext {
                session_approval_covers_high_risk: config.session_approval_covers_high_risk,
                session: lineage.session_approvals.clone(),
                ..ApprovalContext::new(
                    config.policy_mode,
                    config.allowlist.clone(),
                    config.denylist.clone(),
                )
            },
            hooks: {
                let mut hooks = HookRegistry::new(Duration::from_millis(config.hook_timeout_ms));
                for h in &config.hook_commands {
                    hooks.register(Arc::new(
                        arbe_hooks::CommandHook::new(h.phase, h.command.clone())
                            .in_dir(config.project_dir.clone())
                            .with_timeout(h.timeout),
                    ));
                }
                hooks
            },
            events,
            registry,
            history,
            next_turn_index,
            summary,
            skill_instructions,
            allowed_tools: config.tools.clone(),
            startup_warnings,
            redactor: redactor_for(config),
            decisions: lineage.decisions,
        })
        .with_mcp_servers(config.mcp_servers.clone()))
    }

    /// Starts connecting `servers` in the background (a slow server must
    /// not hold up the session); each one's tools appear in the registry
    /// once it's ready, announced by `McpServerConnected`, or its failure
    /// by `McpServerFailed`. Needs a tokio runtime; without one (plain
    /// unit tests) MCP is skipped.
    fn with_mcp_servers(mut self, servers: Vec<arbe_mcp::McpServerConfig>) -> Self {
        if servers.is_empty() {
            return self;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("no async runtime; MCP servers not started");
            return self;
        };
        let manager = Arc::new(McpManager::new(
            servers,
            Some(self.settings.home.join("logs").join("mcp")),
        ));
        let sink = self.registry_sink();
        let events = self.events.clone();
        let connecting = manager.clone();
        runtime.spawn(async move {
            connecting
                .connect_all(&sink, &|status| {
                    events.publish(match status {
                        ServerStatus::Connected { server, tools } => {
                            RuntimeEvent::McpServerConnected { server, tools }
                        }
                        ServerStatus::Failed { server, error } => RuntimeEvent::McpServerFailed {
                            server,
                            reason: error,
                        },
                    })
                })
                .await;
        });
        self.mcp = Some(manager);
        self
    }

    fn registry_sink(&self) -> RegistrySink {
        RegistrySink {
            registry: self.registry.clone(),
            allowed: self.allowed_tools.clone(),
        }
    }

    /// Picks up MCP tool-list changes announced since the last turn.
    async fn refresh_mcp_tools(&self) {
        if let Some(mcp) = &self.mcp {
            mcp.refresh_changed(&self.registry_sink()).await;
        }
    }

    /// Starts a brand new session.
    pub fn create(
        config: &RuntimeConfig,
        store: SessionStore,
        events: Arc<EventBus>,
    ) -> Result<Self, ProviderError> {
        Self::create_with(config, store, events, &ProviderRegistry::with_builtins())
    }

    /// [`create`](Self::create), building the provider from `providers`
    /// (e.g. one with an embedder's own provider registered).
    pub fn create_with(
        config: &RuntimeConfig,
        store: SessionStore,
        events: Arc<EventBus>,
        providers: &ProviderRegistry,
    ) -> Result<Self, ProviderError> {
        let mut meta = store
            .create_session(
                config.profile.clone(),
                config.provider_name.clone(),
                config.model.clone(),
            )
            .map_err(|e| ProviderError::Internal(format!("failed to create session: {e}")))?;
        mark_open(&mut meta, config, &store);
        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        Self::assemble(
            config,
            providers,
            store,
            meta,
            events,
            Vec::new(),
            0,
            None,
            subagent::Lineage::root(config),
        )
    }

    /// A subagent's agent: a new session recording its parent, sharing the
    /// parent tree's approvals and limits (see `subagent`).
    fn create_child(
        config: &RuntimeConfig,
        store: SessionStore,
        events: Arc<EventBus>,
        providers: &ProviderRegistry,
        lineage: subagent::Lineage,
    ) -> Result<Self, ProviderError> {
        let mut meta = store
            .create_session(
                config.profile.clone(),
                config.provider_name.clone(),
                config.model.clone(),
            )
            .map_err(|e| ProviderError::Internal(format!("failed to create session: {e}")))?;
        meta.parent = lineage.parent;
        mark_open(&mut meta, config, &store);
        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        Self::assemble(
            config,
            providers,
            store,
            meta,
            events,
            Vec::new(),
            0,
            None,
            lineage,
        )
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
        Self::resume_with(
            config,
            store,
            session_id,
            events,
            &ProviderRegistry::with_builtins(),
        )
    }

    /// [`resume`](Self::resume), building the provider from `providers`.
    pub fn resume_with(
        config: &RuntimeConfig,
        store: SessionStore,
        session_id: SessionId,
        events: Arc<EventBus>,
        providers: &ProviderRegistry,
    ) -> Result<Self, ProviderError> {
        let mut meta = store
            .resume_session(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to resume session: {e}")))?;
        mark_open(&mut meta, config, &store);
        recover_interrupted_turn(&store, session_id).map_err(|e| {
            ProviderError::Internal(format!("failed to recover interrupted turn: {e}"))
        })?;
        let turns = store
            .list_turns(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to load session history: {e}")))?;
        let (mut history, next_turn_index) = history_from_turns(&turns);
        // Turns covered by the latest compaction are represented by its
        // summary, exactly as they were before the restart.
        let summary = store
            .latest_compaction(session_id)
            .map_err(|e| ProviderError::Internal(format!("failed to load compaction: {e}")))?;
        if let Some(summary) = &summary {
            history.retain(|e| e.turn_index > summary.through_turn_index);
        }

        events.publish(RuntimeEvent::SessionStarted {
            session_id: meta.id,
        });
        Self::assemble(
            config,
            providers,
            store,
            meta,
            events,
            history,
            next_turn_index,
            summary,
            subagent::Lineage::root(config),
        )
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

    /// Problems found while setting up the session (e.g. skill files that
    /// couldn't be loaded), for the UI to show once.
    pub fn startup_warnings(&self) -> &[String] {
        &self.startup_warnings
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
        state.meta.activity = None;
        state.meta.pid = None;
        self.store.save_meta(&state.meta)
    }

    /// The session's title (`meta.json`), if it has one.
    pub fn title(&self) -> Option<String> {
        self.state().meta.title.clone()
    }

    /// Names the session. Without this, the first message sets a title.
    pub fn set_title(&self, title: impl Into<String>) -> Result<(), arbe_storage::StorageError> {
        let mut state = self.state();
        state.meta.title = Some(title.into());
        self.store.save_meta(&state.meta)
    }

    /// Records what the session is doing in `meta.json`.
    fn set_activity(&self, activity: SessionActivity) {
        let mut state = self.state();
        if state.meta.activity == Some(activity) {
            return;
        }
        state.meta.activity = Some(activity);
        if let Err(err) = self.store.save_meta(&state.meta) {
            tracing::warn!(%err, "failed to record session activity");
        }
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
        self.set_activity(SessionActivity::Running);
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

    /// Summarizes all but the newest turn with the model (the `/compact`
    /// command), whatever the memory strategy. Returns whether anything
    /// was compacted.
    pub async fn compact(&self) -> Result<bool, HarnessError> {
        let active = self.begin_exclusive()?;
        Ok(compaction::compact(self, true, &active.cancel)
            .await?
            .is_some())
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

/// Puts an MCP server's tools into the agent's registry (replacing the
/// server's previous set), filtered by the profile's allow-set.
struct RegistrySink {
    registry: Arc<RwLock<Arc<ToolRegistry>>>,
    allowed: Option<Vec<String>>,
}

impl ToolSink for RegistrySink {
    fn replace_server_tools(&self, server: &str, tools: Vec<(String, Arc<dyn ToolExecutor>)>) {
        let prefix = arbe_mcp::server_prefix(server);
        let mut guard = self.registry.write().unwrap_or_else(|p| p.into_inner());
        let registry = Arc::make_mut(&mut guard);
        registry.retain(|name| !name.starts_with(&prefix));
        for (name, executor) in tools {
            if self
                .allowed
                .as_ref()
                .is_none_or(|allowed| tool_allowed(allowed, &name))
            {
                registry.register(name, executor);
            }
        }
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
        self.agent.set_activity(SessionActivity::Idle);
    }
}
