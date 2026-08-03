use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arbe_core::{
    ApprovalDecision, HarnessError, LoopMachine, LoopPhase, MemoryError, Message, ProviderError,
    RiskLevel, Role, RuntimeEvent, SessionId, SessionMeta, SessionStatus, ToolCallId, ToolError,
    ToolInvocation, Turn, TurnId,
};
use arbe_hooks::{HookPhase, HookRegistry};
use arbe_memory::{
    CompactWithSummaryStrategy, ContextPipeline, ContextStrategy, HistoryEntry, TruncationStrategy,
};
use arbe_providers::{ModelProvider, ModelRequest, build_provider};
use arbe_storage::SessionStore;
use arbe_tools::{
    ApprovalContext, ApprovalPolicy, GatedOutcome, StandardApprovalPolicy, ToolExecutor,
    ToolRegistry, execute_gated,
};
use futures_util::StreamExt;
use serde_json::json;

use crate::EventBus;
use crate::config::RuntimeConfig;

fn build_strategy(name: &str) -> Box<dyn ContextStrategy> {
    match name {
        "compact_summary" => Box::new(CompactWithSummaryStrategy),
        _ => Box::new(TruncationStrategy),
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
    last_estimated_tokens: u64,
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
        Ok(Self {
            store,
            meta,
            provider,
            pipeline: ContextPipeline {
                system_instructions: config.system_instructions.clone(),
                ..Default::default()
            },
            strategy: build_strategy(&config.memory_strategy),
            budget_tokens: config.context_budget_tokens,
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            registry: ToolRegistry::new(),
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
            last_estimated_tokens: 0,
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

    /// Runs one full turn: assemble context, stream inference, persist,
    /// emit events. There is no tool-call parsing from the model's output
    /// yet (that needs each provider to surface structured tool calls,
    /// which `ModelResponse`/`TokenChunk` don't model — only plain text
    /// content), so every turn currently takes the direct-response branch
    /// of the loop graph. Tool approval is demoed via
    /// `propose_tool_call`/`resolve_tool_call` instead, which exercise the
    /// exact same `ToolApproval`/`ToolExecution` machinery a parsed tool
    /// call would.
    pub async fn submit_message(&mut self, content: String) -> Result<String, HarnessError> {
        let mut machine = LoopMachine::new();
        let transition = |m: &mut LoopMachine, phase: LoopPhase| {
            m.transition(phase)
                .expect("agent loop transition graph violated");
        };

        transition(&mut machine, LoopPhase::ReceiveUserInput);
        let turn = Turn::new(self.meta.id, self.next_turn_index);
        let turn_id = turn.id;
        self.events.publish(RuntimeEvent::TurnStarted {
            session_id: self.meta.id,
            turn_id,
        });

        transition(&mut machine, LoopPhase::AssembleContext);
        let user_message = Message::new(Role::User, content);
        let context = self.pipeline.assemble(
            self.strategy.as_ref(),
            self.history.clone(),
            self.pinned_turn_indices.clone(),
            user_message.clone(),
            self.budget_tokens,
        );
        self.last_estimated_tokens = context.estimated_tokens;
        self.events.publish(RuntimeEvent::ContextBuilt {
            turn_id,
            estimated_tokens: context.estimated_tokens,
        });

        transition(&mut machine, LoopPhase::PlanOrDirectRespond);
        transition(&mut machine, LoopPhase::ModelInference);

        let request = ModelRequest {
            model: self.meta.model.clone(),
            messages: context.messages,
            temperature: self.temperature,
            max_tokens: self.max_tokens,
        };
        let mut stream = self
            .provider
            .infer_stream(request)
            .await
            .map_err(HarnessError::Provider)?;

        let mut assistant_content = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(HarnessError::Provider)?;
            assistant_content.push_str(&chunk.delta);
            self.events.publish(RuntimeEvent::ModelStreamChunk {
                turn_id,
                delta: chunk.delta,
            });
        }

        transition(&mut machine, LoopPhase::InterpretOutput);
        transition(&mut machine, LoopPhase::PersistTurn);

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

        transition(&mut machine, LoopPhase::EmitEvents);
        self.events.publish(RuntimeEvent::TurnCompleted {
            session_id: self.meta.id,
            turn_id,
        });
        transition(&mut machine, LoopPhase::Idle);

        self.hooks
            .run_phase(
                HookPhase::OnTurnComplete,
                json!({ "turn_id": turn_id.to_string() }),
            )
            .await;

        Ok(assistant_content)
    }

    /// Manually proposes a tool call for approval — a stand-in for
    /// automatic tool-call extraction from model output (not implemented
    /// yet; see `submit_message`'s doc comment). Emits the same
    /// `ToolCallProposed`/`ToolApprovalRequested` events a real one would.
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
            invocation,
            Some(decision),
        )
        .await?;
        if let GatedOutcome::Executed(ref result) = outcome {
            self.events.publish(RuntimeEvent::ToolExecuted {
                turn_id,
                tool_call_id: id,
                result: result.clone(),
            });
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
            last_estimated_tokens: 0,
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
}
