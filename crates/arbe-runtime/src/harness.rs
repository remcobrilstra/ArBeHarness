//! The embedding API (v2 plan P6.1): build a [`Harness`] once, open
//! [`Session`]s from it, and drive each with [`Session::send`], which
//! returns the turn's events as they happen plus its final answer.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use arbe_runtime::harness::Harness;
//! use arbe_runtime::arbe_core::{ApprovalDecision, RuntimeEvent};
//!
//! let harness = Harness::builder().project_dir(".").build()?;
//! let session = harness.new_session()?;
//! let mut turn = session.send("Summarize README.md");
//! while let Some(event) = turn.next_event().await {
//!     match event {
//!         RuntimeEvent::ModelStreamChunk { delta, .. } => print!("{delta}"),
//!         // Nothing runs without a decision: answer every approval.
//!         RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
//!             session.decide(tool_call_id, ApprovalDecision::ApprovedOnce);
//!         }
//!         _ => {}
//!     }
//! }
//! let answer = turn.finish().await?;
//! # Ok(()) }
//! ```
//!
//! Everything a turn does is reported as a [`RuntimeEvent`]; the facade
//! adds no behavior of its own. Tool calls that need approval wait until
//! [`Session::decide`] answers them (or the turn is cancelled).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arbe_core::{
    ApprovalDecision, ConfigError, EventEnvelope, HarnessError, ProviderError, RuntimeEvent,
    SessionId, SessionMeta, ToolCallId,
};
use arbe_providers::{ModelProvider, ProviderRegistry, ProviderSettings};
use arbe_storage::SessionStore;
use arbe_tools::ToolExecutor;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::config::ProfileInfo;
use crate::{Agent, EventBus, RuntimeConfig};

/// How many events a session buffers for a slow reader before the oldest
/// are dropped (the reader then skips ahead; the turn itself never waits).
const EVENT_BUFFER: usize = 4_096;

/// Configures a [`Harness`]. By default configuration is loaded the same
/// way the `arbeharness` binary loads it: built-in defaults, the global
/// and project config files, then `ARBE_*` environment variables.
#[derive(Default)]
pub struct HarnessBuilder {
    config: Option<RuntimeConfig>,
    overrides: HashMap<&'static str, String>,
    extra_config_files: Vec<PathBuf>,
    ignore_env: bool,
    providers: Option<ProviderRegistry>,
    tools: Vec<(String, Arc<dyn ToolExecutor>)>,
}

impl HarnessBuilder {
    /// The directory the agent works in (default: the current directory).
    pub fn project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.overrides
            .insert("ARBE_WORKDIR", dir.into().display().to_string());
        self
    }

    /// Where the harness keeps its own state (default: `~/.arbe`).
    pub fn home(mut self, dir: impl Into<PathBuf>) -> Self {
        self.overrides
            .insert("ARBE_HOME", dir.into().display().to_string());
        self
    }

    /// The settings profile (`coding`, `general`, or one from config).
    pub fn profile(mut self, name: impl Into<String>) -> Self {
        self.overrides.insert("ARBE_PROFILE", name.into());
        self
    }

    pub fn provider(mut self, id: impl Into<String>) -> Self {
        self.overrides.insert("ARBE_PROVIDER", id.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.overrides.insert("ARBE_MODEL", model.into());
        self
    }

    /// Any `ARBE_*` setting by its environment-variable name; wins over
    /// the real environment.
    pub fn setting(mut self, env_name: &'static str, value: impl Into<String>) -> Self {
        self.overrides.insert(env_name, value.into());
        self
    }

    /// An extra config file applied after the global one, with the same
    /// (full) trust — for a program that launches the harness and owns
    /// its own settings, e.g. hooks. A missing file is skipped.
    pub fn config_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.extra_config_files.push(path.into());
        self
    }

    /// Don't read `ARBE_*` from the process environment (only the
    /// builder's own settings and config files apply).
    pub fn ignore_env(mut self) -> Self {
        self.ignore_env = true;
        self
    }

    /// Uses exactly this configuration: no files or environment are read,
    /// and the setters above are ignored.
    pub fn config(mut self, config: RuntimeConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Makes a provider available under `id` (select it with
    /// [`provider`](Self::provider)). The built-in providers stay
    /// available unless `id` replaces one.
    pub fn register_provider<F>(mut self, id: impl Into<String>, factory: F) -> Self
    where
        F: Fn(ProviderSettings) -> Result<Box<dyn ModelProvider>, ProviderError>
            + Send
            + Sync
            + 'static,
    {
        self.providers
            .get_or_insert_with(ProviderRegistry::with_builtins)
            .register(id, factory);
        self
    }

    /// Adds a tool to every session. It goes through the same approval
    /// gate as the built-in tools.
    pub fn tool(mut self, name: impl Into<String>, executor: Arc<dyn ToolExecutor>) -> Self {
        self.tools.push((name.into(), executor));
        self
    }

    pub fn build(self) -> Result<Harness, ConfigError> {
        let (config, source) = match self.config {
            Some(config) => (config, None),
            None => {
                let source = ConfigSource {
                    overrides: self.overrides,
                    extra_files: self.extra_config_files,
                    ignore_env: self.ignore_env,
                };
                let config = source.load(None)?;
                (config, Some(Arc::new(source)))
            }
        };
        Ok(Harness {
            store: SessionStore::with_root(config.home.join("sessions")),
            config,
            providers: self
                .providers
                .unwrap_or_else(ProviderRegistry::with_builtins),
            tools: self.tools,
            source,
        })
    }
}

/// Settings that choose the model service. A profile that sets its own
/// provider or model is used as defined: these are then ignored, even if
/// set in the environment or on the command line — picking that profile
/// is the more specific choice.
const PROVIDER_SETTINGS: [&str; 4] = [
    "ARBE_PROVIDER",
    "ARBE_MODEL",
    "ARBE_BASE_URL",
    "ARBE_API_KEY",
];

/// Where a harness's configuration came from, so it can be resolved again
/// for another profile.
struct ConfigSource {
    overrides: HashMap<&'static str, String>,
    extra_files: Vec<PathBuf>,
    ignore_env: bool,
}

impl ConfigSource {
    fn lookup(&self, key: &str) -> Option<String> {
        self.overrides.get(key).cloned().or_else(|| {
            if self.ignore_env {
                None
            } else {
                std::env::var(key).ok()
            }
        })
    }

    /// The configuration, optionally for `profile` instead of the one the
    /// settings select.
    fn load(&self, profile: Option<&ProfileInfo>) -> Result<RuntimeConfig, ConfigError> {
        let env = |key: &str| match profile {
            Some(p) if key == "ARBE_PROFILE" => Some(p.name.clone()),
            Some(p) if p.sets_provider() && PROVIDER_SETTINGS.contains(&key) => None,
            _ => self.lookup(key),
        };
        RuntimeConfig::load_with(&env, &self.extra_files)
    }

    fn profiles(&self) -> Result<Vec<ProfileInfo>, ConfigError> {
        RuntimeConfig::list_profiles_with(&|key: &str| self.lookup(key), &self.extra_files)
    }
}

/// A configured harness: opens, resumes and lists sessions.
pub struct Harness {
    config: RuntimeConfig,
    store: SessionStore,
    providers: ProviderRegistry,
    tools: Vec<(String, Arc<dyn ToolExecutor>)>,
    /// `None` when built from an explicit [`RuntimeConfig`]: there's
    /// nothing to re-resolve, so profiles can't be switched.
    source: Option<Arc<ConfigSource>>,
}

impl Harness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    /// The resolved configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// Every profile this harness could switch to (see
    /// [`with_profile`](Self::with_profile)), sorted by name.
    pub fn profiles(&self) -> Result<Vec<ProfileInfo>, HarnessError> {
        let source = self.source.as_ref().ok_or_else(|| {
            HarnessError::Internal("this harness was built from a fixed configuration".into())
        })?;
        source
            .profiles()
            .map_err(|e| HarnessError::Internal(e.to_string()))
    }

    /// The same harness configured for another profile: same config files,
    /// environment, registered providers and tools. If the profile sets its
    /// own provider or model, that's what it gets, whatever the environment
    /// or command line says; otherwise those still apply.
    ///
    /// To move a conversation, resume it on the returned harness: history
    /// is provider-neutral, and the session records its new model.
    pub fn with_profile(&self, name: &str) -> Result<Harness, HarnessError> {
        let source = self.source.as_ref().ok_or_else(|| {
            HarnessError::Internal("this harness was built from a fixed configuration".into())
        })?;
        let profiles = self.profiles()?;
        let profile = profiles.iter().find(|p| p.name == name).ok_or_else(|| {
            let known: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
            HarnessError::Internal(format!("no profile {name:?} (known: {})", known.join(", ")))
        })?;
        let config = source
            .load(Some(profile))
            .map_err(|e| HarnessError::Internal(e.to_string()))?;
        Ok(Harness {
            store: SessionStore::with_root(config.home.join("sessions")),
            config,
            providers: self.providers.clone(),
            tools: self.tools.clone(),
            source: self.source.clone(),
        })
    }

    /// Starts a new session.
    pub fn new_session(&self) -> Result<Session, HarnessError> {
        let events = Arc::new(EventBus::new(EVENT_BUFFER));
        let agent = self.create_agent(events.clone())?;
        Ok(Session {
            agent: Arc::new(agent),
            events,
        })
    }

    /// Reopens a saved session, recovering a turn that was interrupted.
    pub fn resume_session(&self, id: SessionId) -> Result<Session, HarnessError> {
        let events = Arc::new(EventBus::new(EVENT_BUFFER));
        let agent = self.resume_agent(id, events.clone())?;
        Ok(Session {
            agent: Arc::new(agent),
            events,
        })
    }

    /// Lower level than [`new_session`](Self::new_session): a new session's
    /// agent publishing to `events` — for a UI that shows one session at a
    /// time on a single event bus.
    pub fn create_agent(&self, events: Arc<EventBus>) -> Result<Agent, HarnessError> {
        let agent = Agent::create_with(&self.config, self.store.clone(), events, &self.providers)?;
        Ok(self.with_tools(agent))
    }

    /// Lower level than [`resume_session`](Self::resume_session); see
    /// [`create_agent`](Self::create_agent).
    pub fn resume_agent(
        &self,
        id: SessionId,
        events: Arc<EventBus>,
    ) -> Result<Agent, HarnessError> {
        let agent = Agent::resume_with(
            &self.config,
            self.store.clone(),
            id,
            events,
            &self.providers,
        )?;
        Ok(self.with_tools(agent))
    }

    /// Every saved top-level session, newest first. Subagents' sessions
    /// (those with a `parent`) are left out; they're in the store.
    pub fn sessions(&self) -> Result<Vec<SessionMeta>, HarnessError> {
        let mut sessions = self
            .store
            .list_sessions()
            .map_err(|e| HarnessError::Internal(e.to_string()))?;
        sessions.retain(|s| s.parent.is_none());
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        Ok(sessions)
    }

    fn with_tools(&self, agent: Agent) -> Agent {
        for (name, executor) in &self.tools {
            agent.register_tool(name.clone(), executor.clone());
        }
        agent
    }
}

/// One open session. Cheap to clone; clones share the session.
#[derive(Clone)]
pub struct Session {
    agent: Arc<Agent>,
    events: Arc<EventBus>,
}

impl Session {
    pub fn id(&self) -> SessionId {
        self.agent.session_id()
    }

    /// The underlying agent, for everything the facade doesn't wrap.
    pub fn agent(&self) -> &Arc<Agent> {
        &self.agent
    }

    /// All of the session's events from now on (turns, MCP status, ...),
    /// independent of any one [`Turn`].
    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events.subscribe()
    }

    /// Sends a user message and starts the turn. Must be called within a
    /// tokio runtime. A second `send` while a turn runs fails that turn
    /// with [`HarnessError::Busy`].
    pub fn send(&self, message: impl Into<String>) -> Turn {
        // Subscribe before starting, so no event can be missed.
        let events = self.events.subscribe();
        let agent = self.agent.clone();
        let message = message.into();
        Turn {
            events,
            task: Some(tokio::spawn(
                async move { agent.submit_message(message).await },
            )),
            result: None,
        }
    }

    /// Answers a `ToolApprovalRequested` event. Returns `false` if nothing
    /// is waiting on `id`.
    pub fn decide(&self, id: ToolCallId, decision: ApprovalDecision) -> bool {
        self.agent.supply_tool_decision(id, decision)
    }

    /// Cancels the running turn, keeping what it produced so far. Returns
    /// whether a turn was running.
    pub fn cancel(&self) -> bool {
        self.agent.cancel_turn()
    }

    /// Names the session (`meta.json`'s `title`).
    pub fn set_title(&self, title: impl Into<String>) -> Result<(), HarnessError> {
        self.agent
            .set_title(title)
            .map_err(|e| HarnessError::Internal(e.to_string()))
    }

    /// Marks the session closed on disk.
    pub fn close(&self) -> Result<(), HarnessError> {
        self.agent
            .close()
            .map_err(|e| HarnessError::Internal(e.to_string()))
    }
}

/// A running turn: its events, then its result.
pub struct Turn {
    events: broadcast::Receiver<EventEnvelope>,
    task: Option<JoinHandle<Result<String, HarnessError>>>,
    result: Option<Result<String, HarnessError>>,
}

impl Turn {
    /// The next event, or `None` once the turn is over and every event it
    /// produced has been returned. If the reader falls far behind, the
    /// oldest events are skipped rather than stalling the turn.
    pub async fn next_event(&mut self) -> Option<RuntimeEvent> {
        loop {
            if let Some(task) = self.task.as_mut() {
                tokio::select! {
                    biased;
                    received = self.events.recv() => match received {
                        Ok(envelope) => return Some(envelope.event),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(skipped, "turn event reader fell behind");
                        }
                        Err(broadcast::error::RecvError::Closed) => return None,
                    },
                    joined = task => {
                        self.result = Some(joined.unwrap_or_else(|e| {
                            Err(HarnessError::Internal(format!("turn task failed: {e}")))
                        }));
                        self.task = None;
                    }
                }
            } else {
                // The turn is over: return what's still buffered, then end.
                return match self.events.try_recv() {
                    Ok(envelope) => Some(envelope.event),
                    Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                    Err(_) => None,
                };
            }
        }
    }

    /// Waits for the turn to end (skipping remaining events) and returns
    /// the final answer.
    pub async fn finish(mut self) -> Result<String, HarnessError> {
        while self.next_event().await.is_some() {}
        self.result
            .unwrap_or_else(|| Err(HarnessError::Internal("turn result missing".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{StopReason, Usage};
    use arbe_providers::{
        CancellationToken, ModelCapabilities, ModelRequest, ProviderEvent, ProviderStream,
    };
    use async_trait::async_trait;

    /// Answers every request with "echo: <last user message>".
    struct EchoProvider;

    #[async_trait]
    impl ModelProvider for EchoProvider {
        fn id(&self) -> &str {
            "echo"
        }
        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calls: false,
                vision: false,
                thinking: false,
                prompt_caching: false,
                max_context_tokens: 32_000,
            }
        }
        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            let last = req.messages.last().map(|m| m.text()).unwrap_or_default();
            let events = vec![
                Ok(ProviderEvent::TextDelta(format!("echo: {last}"))),
                Ok(ProviderEvent::Usage(Usage::default())),
                Ok(ProviderEvent::Stop(StopReason::EndTurn)),
            ];
            Ok(Box::pin(futures_util::stream::iter(events)))
        }
    }

    fn harness(home: &std::path::Path, project: &std::path::Path) -> Harness {
        Harness::builder()
            .ignore_env()
            .home(home)
            .project_dir(project)
            .register_provider("echo", |_| {
                Ok(Box::new(EchoProvider) as Box<dyn ModelProvider>)
            })
            .provider("echo")
            .model("any")
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_turn_streams_its_events_then_returns_the_answer() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let harness = harness(home.path(), project.path());
        assert_eq!(harness.config().home, home.path());

        let session = harness.new_session().unwrap();
        let mut turn = session.send("hi");
        let mut streamed = String::new();
        let mut completed = false;
        while let Some(event) = turn.next_event().await {
            match event {
                RuntimeEvent::ModelStreamChunk { delta, .. } => streamed.push_str(&delta),
                RuntimeEvent::TurnCompleted { .. } => completed = true,
                _ => {}
            }
        }
        assert!(completed);
        assert_eq!(streamed, "echo: hi");
        assert_eq!(turn.finish().await.unwrap(), "echo: hi");

        // Stored under the harness home, listed, and resumable.
        let listed = harness.sessions().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, session.id());
        assert_eq!(listed[0].title.as_deref(), Some("hi"));
        assert!(
            home.path()
                .join("sessions")
                .join(session.id().to_string())
                .is_dir()
        );
        session.close().unwrap();
        let resumed = harness.resume_session(session.id()).unwrap();
        assert_eq!(resumed.send("again").finish().await.unwrap(), "echo: again");
    }

    fn write_global_config(home: &std::path::Path, toml: &str) {
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::write(home.join("config").join("config.toml"), toml).unwrap();
    }

    #[test]
    fn profiles_list_the_built_ins_and_the_configured_ones() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global_config(
            home.path(),
            "[profiles.fast]\nprovider = { name = \"echo\", model = \"small\" }\n\n[profiles.careful]\nprompt = \"general\"\n",
        );
        let profiles = harness(home.path(), project.path()).profiles().unwrap();
        let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["careful", "coding", "fast", "general"]);
        let fast = profiles.iter().find(|p| p.name == "fast").unwrap();
        assert_eq!(
            (fast.provider.as_deref(), fast.model.as_deref()),
            (Some("echo"), Some("small"))
        );
        assert!(fast.sets_provider() && !fast.builtin);
        let careful = profiles.iter().find(|p| p.name == "careful").unwrap();
        assert!(!careful.sets_provider());
        assert!(
            profiles
                .iter()
                .find(|p| p.name == "coding")
                .unwrap()
                .builtin
        );
    }

    #[tokio::test]
    async fn switching_profile_moves_the_conversation_to_its_model() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_global_config(
            home.path(),
            "[profiles.big]\nprovider = { name = \"echo\", model = \"big-model\" }\n\n[profiles.plain]\nprompt = \"general\"\n",
        );
        // The builder's provider/model stand in for ARBE_PROVIDER/ARBE_MODEL.
        let harness = harness(home.path(), project.path());
        assert_eq!(harness.config().model, "any");
        let session = harness.new_session().unwrap();
        session.send("first").finish().await.unwrap();
        session.close().unwrap();

        // A profile with its own model wins over the environment's.
        let big = harness.with_profile("big").unwrap();
        assert_eq!(
            (big.config().profile.as_str(), big.config().model.as_str()),
            ("big", "big-model")
        );
        let moved = big.resume_session(session.id()).unwrap();
        assert_eq!(moved.agent().model(), "big-model");
        assert_eq!(moved.send("second").finish().await.unwrap(), "echo: second");
        let meta = big.store().load_meta(session.id()).unwrap();
        assert_eq!(
            (meta.profile.as_str(), meta.model.as_str()),
            ("big", "big-model")
        );
        assert_eq!(big.store().list_turns(session.id()).unwrap().len(), 2);

        // One without keeps the environment's provider and model.
        let plain = big.with_profile("plain").unwrap();
        assert_eq!(plain.config().model, "any");
        assert_eq!(plain.config().provider_name, "echo");

        let err = harness.with_profile("nope").err().unwrap().to_string();
        assert!(err.contains("known: big, coding, general, plain"), "{err}");
    }

    #[test]
    fn a_harness_built_from_a_fixed_config_cannot_switch() {
        let project = tempfile::tempdir().unwrap();
        let harness = Harness::builder()
            .config(RuntimeConfig::defaults(project.path().to_path_buf()))
            .build()
            .unwrap();
        assert!(harness.profiles().is_err());
        assert!(harness.with_profile("general").is_err());
    }
}
