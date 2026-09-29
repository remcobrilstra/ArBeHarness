use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arbe_core::{
    ApprovalDecision, ApprovalPolicyMode, ContentBlock, HarnessError, HookError, Message,
    ProviderError, Role, RuntimeEvent, SessionId, StopReason, ToolError, ToolInvocation,
    ToolResult, Turn, Usage,
};
use arbe_hooks::{Hook, HookPhase, HookRegistry};
use arbe_memory::{ContextPipeline, HistoryEntry};
use arbe_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent,
    ProviderStream, RetryPolicy,
};
use arbe_storage::{InFlightMessage, SessionStore};
use arbe_tools::{ApprovalContext, ToolContext, ToolExecutor, ToolRegistry};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::*;
use crate::EventBus;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

/// One scripted model response.
#[derive(Clone)]
enum Round {
    Events(Vec<ProviderEvent>),
    /// Emits the events, then never finishes (until cancelled).
    Hang(Vec<ProviderEvent>),
}

fn usage_event() -> ProviderEvent {
    ProviderEvent::Usage(Usage {
        input_tokens: 10,
        output_tokens: 5,
        ..Default::default()
    })
}

fn answer(text: &str) -> Round {
    Round::Events(vec![
        ProviderEvent::TextDelta(text.to_string()),
        usage_event(),
        ProviderEvent::Stop(StopReason::EndTurn),
    ])
}

/// A round requesting `calls` (`(provider id, tool name, arguments)`),
/// with the arguments streamed in two fragments each.
fn tool_calls(calls: &[(&str, &str, Value)]) -> Round {
    let mut events = vec![ProviderEvent::TextDelta("Let me check.".to_string())];
    for (id, name, args) in calls {
        let json = args.to_string();
        let (a, b) = json.split_at(json.len() / 2);
        events.push(ProviderEvent::ToolUseStart {
            id: id.to_string(),
            name: name.to_string(),
        });
        for part in [a, b] {
            events.push(ProviderEvent::ToolUseInputDelta {
                id: id.to_string(),
                partial_json: part.to_string(),
            });
        }
        events.push(ProviderEvent::ToolUseEnd { id: id.to_string() });
    }
    events.push(usage_event());
    events.push(ProviderEvent::Stop(StopReason::ToolUse));
    Round::Events(events)
}

/// Replays scripted rounds in order (then `fallback` forever), recording
/// every request it receives.
struct ScriptedProvider {
    rounds: Mutex<VecDeque<Round>>,
    fallback: Round,
    tool_calls: bool,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ScriptedProvider {
    fn new(rounds: Vec<Round>, fallback: Round) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
            fallback,
            tool_calls: true,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn answering(text: &str) -> Self {
        Self {
            tool_calls: false,
            ..Self::new(vec![], answer(text))
        }
    }
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calls: self.tool_calls,
            vision: false,
            thinking: false,
            prompt_caching: false,
            max_context_tokens: 8_000,
        }
    }

    async fn stream(
        &self,
        req: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        let round = self
            .rounds
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone());
        let stream: ProviderStream = match round {
            Round::Events(events) => {
                Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
            }
            Round::Hang(events) => Box::pin(
                futures_util::stream::iter(events.into_iter().map(Ok::<_, ProviderError>))
                    .chain(futures_util::stream::pending()),
            ),
        };
        Ok(arbe_providers::http::cancellable(stream, cancel))
    }
}

struct FailingProvider;

#[async_trait]
impl ModelProvider for FailingProvider {
    fn id(&self) -> &str {
        "failing"
    }

    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ScriptedProvider::answering("").capabilities("")
    }

    async fn stream(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        Err(ProviderError::Auth("bad key".to_string()))
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

/// Sleeps (cancellably) and records when it ran.
struct SlowTool {
    delay: Duration,
    runs: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

#[async_trait]
impl ToolExecutor for SlowTool {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let start = Instant::now();
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            _ = tokio::time::sleep(self.delay) => {}
        }
        self.runs.lock().unwrap().push((start, Instant::now()));
        Ok(ToolResult {
            id: invocation.id,
            output: json!("slept"),
            is_error: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct TestAgent {
    agent: Arc<Agent>,
    store: SessionStore,
    session_id: SessionId,
    dir: PathBuf,
}

impl Drop for TestAgent {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn temp_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("arbe-agent-{label}-{}", uuid::Uuid::new_v4()))
}

fn test_parts(
    store: SessionStore,
    meta: arbe_core::SessionMeta,
    events: Arc<EventBus>,
    provider: Box<dyn ModelProvider>,
) -> Parts {
    Parts {
        settings: Settings {
            session_id: meta.id,
            profile: "default".into(),
            provider_name: "fake".into(),
            model: "fake-model".into(),
            temperature: 0.2,
            max_tokens: 100,
            budget_tokens: 8_000,
            max_tool_rounds: 50,
            max_turn_tokens: None,
            max_tool_output_chars: 50_000,
            auto_compact: false,
            retry: RetryPolicy::none(),
            thinking_budget_tokens: None,
            // Doesn't exist: no project instruction files, so tests don't
            // depend on this repo's own CLAUDE.md.
            project_dir: temp_dir("no-project"),
            prompt: crate::system_prompt::PromptTemplate::Coding,
            home: temp_dir("no-home"),
        },
        store,
        meta,
        provider,
        strategy: build_strategy("truncation"),
        approval_ctx: ApprovalContext::new(ApprovalPolicyMode::AlwaysPrompt, vec![], vec![]),
        hooks: HookRegistry::new(Duration::from_millis(500)),
        events,
        registry: ToolRegistry::new(),
        history: Vec::new(),
        next_turn_index: 0,
        summary: None,
        skill_instructions: Vec::new(),
        allowed_tools: None,
        startup_warnings: Vec::new(),
        redactor: crate::redact::Redactor::default(),
        decisions: Default::default(),
    }
}

fn test_agent_with(
    provider: impl ModelProvider + 'static,
    configure: impl FnOnce(&mut Parts),
) -> (
    TestAgent,
    tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>,
) {
    let dir = temp_dir("store");
    let store = SessionStore::with_root(dir.clone());
    let meta = store
        .create_session("default", "fake", "fake-model")
        .unwrap();
    let events = Arc::new(EventBus::new(4_096));
    let rx = events.subscribe();
    let mut parts = test_parts(store.clone(), meta.clone(), events, Box::new(provider));
    configure(&mut parts);
    let agent = Arc::new(Agent::from_parts(parts));
    (
        TestAgent {
            agent,
            store,
            session_id: meta.id,
            dir,
        },
        rx,
    )
}

fn test_agent(provider: impl ModelProvider + 'static) -> TestAgent {
    test_agent_with(provider, |_| {}).0
}

fn allow(tools: &[&str]) -> impl FnOnce(&mut Parts) {
    let tools: Vec<String> = tools.iter().map(|t| t.to_string()).collect();
    move |parts| {
        parts.approval_ctx = ApprovalContext::new(ApprovalPolicyMode::AllowlistAuto, tools, vec![]);
    }
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>) -> Vec<RuntimeEvent> {
    let mut out = Vec::new();
    while let Ok(envelope) = rx.try_recv() {
        out.push(envelope.event);
    }
    out
}

async fn wait_for<T>(
    rx: &mut tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>,
    mut pick: impl FnMut(RuntimeEvent) -> Option<T>,
) -> T {
    loop {
        let envelope = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for an event")
            .unwrap();
        if let Some(found) = pick(envelope.event) {
            return found;
        }
    }
}

fn tool_result_of(message: &Message, index: usize) -> (String, String, bool) {
    match &message.content[index] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => (
            tool_use_id.clone(),
            Message::with_blocks(Role::Tool, content.clone()).text(),
            *is_error,
        ),
        other => panic!("expected a tool result, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Basic turns
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_turn_streams_persists_and_emits_events() {
    let (t, mut rx) = test_agent_with(ScriptedProvider::answering("hello"), |_| {});
    let reply = t.agent.submit_message("hi there".into()).await.unwrap();
    assert_eq!(reply, "hello");

    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].final_assistant_message().unwrap().text(), "hello");
    assert_eq!(turns[0].stop_reason, Some(StopReason::EndTurn));
    // The in-flight log is cleared once the turn is committed.
    assert!(t.store.read_in_flight(t.session_id).unwrap().is_empty());

    let events = drain(&mut rx);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ModelStreamChunk { delta, .. } if delta == "hello"))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        RuntimeEvent::TurnCompleted {
            stop_reason: StopReason::EndTurn,
            ..
        }
    )));
    assert!(!t.agent.is_busy());
}

#[tokio::test]
async fn later_turns_see_earlier_turns_in_history() {
    let t = test_agent(ScriptedProvider::answering("ok"));
    t.agent.submit_message("first".into()).await.unwrap();
    t.agent.submit_message("second".into()).await.unwrap();
    let state = t.agent.state();
    assert_eq!(state.history.len(), 4);
    assert_eq!(state.next_turn_index, 2);
}

#[tokio::test]
async fn project_instructions_are_reread_on_every_turn() {
    let project_dir = temp_dir("project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let dir_for_parts = project_dir.clone();
    let (t, _rx) = test_agent_with(ScriptedProvider::answering("ok"), move |parts| {
        parts.settings.project_dir = dir_for_parts;
    });

    t.agent.submit_message("first".into()).await.unwrap();
    assert!(!t.agent.state().pipeline.system_instructions[0].contains("edited mid-session"));

    std::fs::write(project_dir.join("agent.md"), "edited mid-session").unwrap();
    t.agent.submit_message("second".into()).await.unwrap();
    assert!(t.agent.state().pipeline.system_instructions[0].contains("edited mid-session"));

    std::fs::remove_dir_all(&project_dir).ok();
}

#[tokio::test]
async fn a_failed_turn_publishes_its_error_and_leaves_no_turn_behind() {
    let (t, mut rx) = test_agent_with(FailingProvider, |_| {});
    let err = t.agent.submit_message("hi".into()).await.unwrap_err();
    assert!(matches!(
        err,
        HarnessError::Provider(ProviderError::Auth(_))
    ));

    let events = drain(&mut rx);
    let started = events.iter().find_map(|e| match e {
        RuntimeEvent::TurnStarted { turn_id, .. } => Some(*turn_id),
        _ => None,
    });
    let errored = events.iter().find_map(|e| match e {
        RuntimeEvent::RuntimeError { turn_id, reason } => {
            assert!(reason.contains("bad key"));
            *turn_id
        }
        _ => None,
    });
    assert!(started.is_some());
    assert_eq!(started, errored);
    // Nothing happened beyond the question: no turn record, so a retry
    // doesn't repeat it in history.
    assert!(t.store.list_turns(t.session_id).unwrap().is_empty());
    assert!(t.store.read_in_flight(t.session_id).unwrap().is_empty());
}

#[tokio::test]
async fn usage_is_recorded_on_the_turn_and_the_session_and_published() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({"x": 1}))])],
        answer("done"),
    );
    let (t, mut rx) = test_agent_with(provider, allow(&["echo"]));
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("use echo".into()).await.unwrap();

    // Two inference rounds of 10 in / 5 out each.
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].usage.input_tokens, 20);
    assert_eq!(turns[0].usage.output_tokens, 10);
    assert_eq!(
        t.store
            .load_meta(t.session_id)
            .unwrap()
            .usage
            .total_tokens(),
        30
    );
    assert_eq!(t.agent.usage().total_tokens(), 30);
    let published = drain(&mut rx).into_iter().find_map(|e| match e {
        RuntimeEvent::UsageUpdated { session, .. } => Some(session),
        _ => None,
    });
    assert_eq!(published.unwrap().total_tokens(), 30);
}

#[tokio::test]
async fn reported_input_tokens_calibrate_the_estimator() {
    let big_input = Round::Events(vec![
        ProviderEvent::TextDelta("ok".into()),
        ProviderEvent::Usage(Usage {
            input_tokens: 10_000_000,
            output_tokens: 1,
            ..Default::default()
        }),
    ]);
    let t = test_agent(ScriptedProvider::new(vec![], big_input));
    t.agent.submit_message("hi".into()).await.unwrap();
    let first = t.agent.state().calibration.factor();
    assert!(first > 1.0, "factor {first}");
    t.agent.submit_message("hi".into()).await.unwrap();
    assert!(t.agent.state().calibration.factor() > first);
}

// ---------------------------------------------------------------------------
// Tool rounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_whole_tool_trace_is_sent_persisted_and_replayed() {
    let provider = ScriptedProvider::new(
        vec![
            tool_calls(&[("c1", "echo", json!({"x": 1}))]),
            answer("final answer"),
        ],
        answer("second turn answer"),
    );
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, allow(&["echo"]));
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    let reply = t.agent.submit_message("use echo".into()).await.unwrap();
    assert_eq!(reply, "final answer");

    {
        let requests = requests.lock().unwrap();
        // Tools registered at runtime are offered to the model, with the
        // description each tool gives of itself.
        assert!(requests[0].tools.iter().any(|t| t.name == "echo"));
        let second = &requests[1].messages;
        let assistant = &second[second.len() - 2];
        // Text alongside the tool call is kept; fragments were reassembled.
        assert_eq!(assistant.text(), "Let me check.");
        assert_eq!(assistant.tool_uses()[0].arguments, json!({"x": 1}));
        let (id, text, is_error) = tool_result_of(second.last().unwrap(), 0);
        assert_eq!(id, "c1");
        assert_eq!(text, r#"{"x":1}"#);
        assert!(!is_error);
    }

    // Persisted: user, assistant(tool call), tool result, final answer.
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].messages.len(), 4);

    // Replayed: the next turn's request carries the previous turn's trace.
    t.agent.submit_message("and now?".into()).await.unwrap();
    let requests = requests.lock().unwrap();
    let third = &requests[2].messages;
    assert!(third.iter().any(|m| m.has_tool_uses()));
    assert!(third.iter().any(|m| m.role == Role::Tool));
}

#[tokio::test]
async fn a_failing_tool_call_goes_back_to_the_model_instead_of_failing_the_turn() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({}))])],
        answer("recovered"),
    );
    let requests = provider.requests.clone();
    // `echo` is deliberately not registered: the call fails validation.
    let t = test_agent(provider);

    assert_eq!(
        t.agent.submit_message("go".into()).await.unwrap(),
        "recovered"
    );
    let requests = requests.lock().unwrap();
    let (_, text, is_error) = tool_result_of(requests[1].messages.last().unwrap(), 0);
    assert!(is_error);
    assert!(text.contains("no tool registered"));
}

#[tokio::test]
async fn a_denied_call_is_reported_to_the_model_which_then_answers() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({}))])],
        answer("I couldn't run echo."),
    );
    let requests = provider.requests.clone();
    let (t, mut rx) = test_agent_with(provider, |parts| {
        parts.approval_ctx = ApprovalContext::new(
            ApprovalPolicyMode::DenylistBlock,
            vec![],
            vec!["echo".into()],
        );
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    let reply = t.agent.submit_message("use echo".into()).await.unwrap();
    assert_eq!(reply, "I couldn't run echo.");
    let requests = requests.lock().unwrap();
    let (_, text, is_error) = tool_result_of(requests[1].messages.last().unwrap(), 0);
    assert!(is_error);
    assert_eq!(text, "denied by approval policy");
    assert!(
        drain(&mut rx)
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolCallDenied { .. }))
    );
}

#[tokio::test]
async fn an_approval_prompt_is_answered_while_the_turn_is_running() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({"x": 1}))])],
        answer("final answer"),
    );
    // Default policy: AlwaysPrompt.
    let (t, mut rx) = test_agent_with(provider, |_| {});
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    let agent = t.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("use echo".into()).await });
    let id = wait_for(&mut rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    assert!(t.agent.is_busy());
    assert!(
        t.agent
            .supply_tool_decision(id, ApprovalDecision::ApprovedOnce)
    );

    let reply = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("turn never finished")
        .unwrap()
        .unwrap();
    assert_eq!(reply, "final answer");
}

#[tokio::test]
async fn parallel_safe_calls_run_concurrently_and_results_keep_the_models_order() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[
            ("a", "slow", json!({"n": 1})),
            ("b", "slow", json!({"n": 2})),
        ])],
        answer("done"),
    );
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, allow(&["slow"]));
    t.agent.register_tool(
        "slow",
        Arc::new(SlowTool {
            delay: Duration::from_millis(300),
            runs: runs.clone(),
        }),
    );

    t.agent.submit_message("go".into()).await.unwrap();

    let runs = runs.lock().unwrap();
    assert_eq!(runs.len(), 2);
    let (first, second) = (runs[0], runs[1]);
    // Overlapping intervals: the second started before the first ended.
    assert!(
        second.0 < first.1 && first.0 < second.1,
        "calls ran sequentially"
    );

    let requests = requests.lock().unwrap();
    let results = requests[1].messages.last().unwrap();
    assert_eq!(tool_result_of(results, 0).0, "a");
    assert_eq!(tool_result_of(results, 1).0, "b");
}

#[tokio::test]
async fn before_tool_execute_hooks_can_rewrite_or_veto_a_call() {
    struct Rewrite;
    #[async_trait]
    impl Hook for Rewrite {
        fn phase(&self) -> HookPhase {
            HookPhase::BeforeToolExecute
        }
        async fn run(&self, mut payload: Value) -> Result<Value, HookError> {
            if payload["tool_name"] == "echo" {
                payload["arguments"] = json!({"rewritten": true});
            } else {
                payload["veto"] = json!("not allowed today");
            }
            Ok(payload)
        }
    }

    let provider = ScriptedProvider::new(
        vec![tool_calls(&[
            ("a", "echo", json!({"original": true})),
            ("b", "other", json!({})),
        ])],
        answer("done"),
    );
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, |parts| {
        allow(&["echo", "other"])(parts);
        parts.hooks.register(Arc::new(Rewrite));
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));
    t.agent.register_tool("other", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();
    let requests = requests.lock().unwrap();
    let results = requests[1].messages.last().unwrap();
    assert_eq!(tool_result_of(results, 0).1, r#"{"rewritten":true}"#);
    let (_, text, is_error) = tool_result_of(results, 1);
    assert!(is_error);
    assert_eq!(text, "blocked by hook: not allowed today");
}

#[tokio::test]
async fn long_tool_output_is_truncated_before_it_reaches_the_model() {
    let big = "x".repeat(10_000);
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!(big))])],
        answer("done"),
    );
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, |parts| {
        allow(&["echo"])(parts);
        parts.settings.max_tool_output_chars = 1_000;
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();
    let requests = requests.lock().unwrap();
    let (_, text, _) = tool_result_of(requests[1].messages.last().unwrap(), 0);
    assert!(text.chars().count() < 1_100);
    assert!(text.contains("characters omitted"));
}

#[tokio::test]
async fn manual_tool_invocations_use_the_same_gated_path() {
    let (t, mut rx) = test_agent_with(ScriptedProvider::answering(""), |_| {});
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    let agent = t.agent.clone();
    let call = tokio::spawn(async move { agent.invoke_tool("echo", json!({"x": 1})).await });
    let id = wait_for(&mut rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    assert!(
        t.agent
            .supply_tool_decision(id, ApprovalDecision::ApprovedOnce)
    );
    let message = call.await.unwrap().unwrap();
    let (_, text, is_error) = tool_result_of(&message, 0);
    assert_eq!(text, r#"{"x":1}"#);
    assert!(!is_error);
}

// ---------------------------------------------------------------------------
// Loop guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn repeating_the_identical_tool_call_stops_the_turn() {
    let same = tool_calls(&[("c", "echo", json!({"x": 1}))]);
    let (t, mut rx) = test_agent_with(ScriptedProvider::new(vec![], same), allow(&["echo"]));
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].stop_reason, Some(StopReason::RepeatedToolCall));
    let executed = drain(&mut rx)
        .iter()
        .filter(|e| matches!(e, RuntimeEvent::ToolExecuted { .. }))
        .count();
    assert_eq!(executed, 2, "the third identical round must not run");
    // The unanswered third call was closed so history stays valid.
    let last = turns[0].messages.last().unwrap();
    assert!(tool_result_of(last, 0).1.contains("not executed"));
}

#[tokio::test]
async fn the_tool_round_limit_stops_the_turn() {
    let rounds = (0..10)
        .map(|i| tool_calls(&[("c", "echo", json!({ "i": i }))]))
        .collect();
    let (t, _rx) = test_agent_with(ScriptedProvider::new(rounds, answer("never")), |parts| {
        allow(&["echo"])(parts);
        parts.settings.max_tool_rounds = 3;
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].stop_reason, Some(StopReason::ToolRoundLimit));
}

#[tokio::test]
async fn the_turn_token_ceiling_stops_the_turn() {
    let rounds = (0..10)
        .map(|i| tool_calls(&[("c", "echo", json!({ "i": i }))]))
        .collect();
    let (t, _rx) = test_agent_with(ScriptedProvider::new(rounds, answer("never")), |parts| {
        allow(&["echo"])(parts);
        // Each round reports 15 tokens.
        parts.settings.max_turn_tokens = Some(40);
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].stop_reason, Some(StopReason::TurnTokenLimit));
    assert_eq!(turns[0].usage.total_tokens(), 45);
}

// ---------------------------------------------------------------------------
// Cancellation and concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelling_mid_stream_keeps_the_partial_answer_and_frees_the_session() {
    let provider = ScriptedProvider::new(
        vec![Round::Hang(vec![ProviderEvent::TextDelta(
            "partial ans".into(),
        )])],
        answer("after cancel"),
    );
    let (t, mut rx) = test_agent_with(provider, |_| {});

    let agent = t.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("long question".into()).await });
    wait_for(&mut rx, |e| {
        matches!(e, RuntimeEvent::ModelStreamChunk { .. }).then_some(())
    })
    .await;
    assert!(t.agent.cancel_turn());

    let result = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("cancel didn't stop the turn")
        .unwrap();
    assert!(matches!(result, Err(HarnessError::Cancelled)));
    assert!(
        drain(&mut rx)
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnCancelled { .. }))
    );

    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns[0].stop_reason, Some(StopReason::Cancelled));
    assert_eq!(
        turns[0].final_assistant_message().unwrap().text(),
        "partial ans"
    );

    // The session is usable again straight away.
    assert!(!t.agent.is_busy());
    assert!(!t.agent.cancel_turn());
    assert_eq!(
        t.agent.submit_message("next".into()).await.unwrap(),
        "after cancel"
    );
}

#[tokio::test]
async fn cancelling_while_awaiting_approval_records_the_call_as_not_run() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({}))])],
        answer("unused"),
    );
    let (t, mut rx) = test_agent_with(provider, |_| {});
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    let agent = t.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("go".into()).await });
    let id = wait_for(&mut rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    t.agent.cancel_turn();
    assert!(matches!(turn.await.unwrap(), Err(HarnessError::Cancelled)));
    // The prompt is gone: a late answer isn't delivered anywhere.
    assert!(
        !t.agent
            .supply_tool_decision(id, ApprovalDecision::ApprovedOnce)
    );

    let turns = t.store.list_turns(t.session_id).unwrap();
    let (_, text, is_error) = tool_result_of(turns[0].messages.last().unwrap(), 0);
    assert!(is_error);
    assert!(text.contains("cancelled before this tool call ran"));
}

#[tokio::test]
async fn cancelling_stops_running_tools() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "slow", json!({}))])],
        answer("unused"),
    );
    let (t, mut rx) = test_agent_with(provider, allow(&["slow"]));
    t.agent.register_tool(
        "slow",
        Arc::new(SlowTool {
            delay: Duration::from_secs(30),
            runs: runs.clone(),
        }),
    );

    let agent = t.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("go".into()).await });
    wait_for(&mut rx, |e| {
        matches!(e, RuntimeEvent::ToolCallProposed { .. }).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    t.agent.cancel_turn();
    let result = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("a running tool kept the turn alive")
        .unwrap();
    assert!(matches!(result, Err(HarnessError::Cancelled)));
    assert!(
        runs.lock().unwrap().is_empty(),
        "the tool ran to completion"
    );
}

#[tokio::test]
async fn a_second_submission_during_a_turn_is_busy() {
    let provider = ScriptedProvider::new(vec![Round::Hang(vec![])], answer("ok"));
    let (t, mut rx) = test_agent_with(provider, |_| {});
    let agent = t.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("one".into()).await });
    wait_for(&mut rx, |e| {
        matches!(e, RuntimeEvent::ContextBuilt { .. }).then_some(())
    })
    .await;

    assert!(matches!(
        t.agent.submit_message("two".into()).await,
        Err(HarnessError::Busy)
    ));
    t.agent.cancel_turn();
    let _ = turn.await;
}

// ---------------------------------------------------------------------------
// Persistence and recovery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_resumes_with_its_full_history() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({}))])],
        answer("done"),
    );
    let (t, _rx) = test_agent_with(provider, allow(&["echo"]));
    t.agent.register_tool("echo", Arc::new(EchoExecutor));
    t.agent
        .submit_message("before the crash".into())
        .await
        .unwrap();

    // A second "process" rebuilds from disk, as `Agent::resume` does.
    let turns = t.store.list_turns(t.session_id).unwrap();
    let (history, next) = history_from_turns(&turns);
    assert_eq!(history.len(), 4);
    assert_eq!(next, 1);
    assert!(history.iter().all(|e| e.turn_index == 0));
}

#[tokio::test]
async fn an_interrupted_turn_is_recovered_from_the_in_flight_log() {
    let t = test_agent(ScriptedProvider::answering("ok"));
    t.agent
        .submit_message("a completed turn".into())
        .await
        .unwrap();

    // Simulate a process that died mid-turn, after the model asked for a
    // tool but before it ran.
    let turn_id = arbe_core::TurnId::new();
    let call = arbe_core::RequestedToolCall {
        id: "c9".into(),
        name: "execute".into(),
        arguments: json!({"command": "make"}),
    };
    for message in [
        Message::new(Role::User, "build it"),
        Message::assistant_tool_calls(vec![call]),
    ] {
        t.store
            .append_in_flight(
                t.session_id,
                &InFlightMessage {
                    turn_id,
                    turn_index: 1,
                    message,
                },
            )
            .unwrap();
    }

    recover_interrupted_turn(&t.store, t.session_id).unwrap();
    let turns = t.store.list_turns(t.session_id).unwrap();
    assert_eq!(turns.len(), 2);
    let recovered: &Turn = &turns[1];
    assert_eq!(recovered.id, turn_id);
    assert_eq!(recovered.stop_reason, Some(StopReason::Interrupted));
    // user, assistant(tool call), synthesized error result.
    assert_eq!(recovered.messages.len(), 3);
    assert!(tool_result_of(&recovered.messages[2], 0).2);
    assert!(t.store.read_in_flight(t.session_id).unwrap().is_empty());

    // Recovering again is a no-op.
    recover_interrupted_turn(&t.store, t.session_id).unwrap();
    assert_eq!(t.store.list_turns(t.session_id).unwrap().len(), 2);

    let (history, next) = history_from_turns(&turns);
    assert_eq!(history.len(), 5);
    assert_eq!(next, 2);
}

#[test]
fn memory_strategy_is_swappable_via_config_alone() {
    // Same history, same tiny budget, only the configured strategy differs.
    let history = vec![
        HistoryEntry {
            turn_index: 0,
            message: Message::new(Role::User, "a".repeat(200)),
        },
        HistoryEntry {
            turn_index: 1,
            message: Message::new(Role::Assistant, "b".repeat(200)),
        },
    ];
    let assemble = |name: &str| {
        ContextPipeline::default().assemble(
            build_strategy(name).as_ref(),
            &history,
            &[],
            Message::new(Role::User, "go"),
            5,
        )
    };
    let compacted = |out: &arbe_memory::ContextOutput| {
        out.messages.iter().any(|m| m.text().contains("compacted"))
    };
    assert!(!compacted(&assemble("truncation")));
    assert!(compacted(&assemble("compact_summary")));
}

/// The real `Agent::create` path (provider built from config, builtin
/// tools registered against the project dir), driven through a manual
/// tool call so no model is needed.
#[tokio::test]
async fn builtin_tools_are_registered_and_usable_through_the_real_agent() {
    let store_dir = temp_dir("real-store");
    let project_dir = temp_dir("real-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("marker.txt"), "hi").unwrap();

    let config = crate::RuntimeConfig {
        provider_name: "ollama".into(),
        project_dir: project_dir.clone(),
        policy_mode: ApprovalPolicyMode::AllowlistAuto,
        allowlist: vec!["list_dir".into()],
        ..crate::RuntimeConfig::from_env()
    };
    let agent = Agent::create(
        &config,
        SessionStore::with_root(store_dir.clone()),
        Arc::new(EventBus::default()),
    )
    .unwrap();

    let message = agent.invoke_tool("list_dir", json!({})).await.unwrap();
    let (_, text, is_error) = tool_result_of(&message, 0);
    assert!(!is_error, "{text}");
    assert!(text.contains("marker.txt"));

    std::fs::remove_dir_all(&store_dir).ok();
    std::fs::remove_dir_all(&project_dir).ok();
}

/// A profile's tool allow-set is enforced by construction: tools outside
/// it aren't registered, so they're neither offered nor callable.
#[tokio::test]
async fn the_general_profile_agent_only_has_its_allowed_tools() {
    let store_dir = temp_dir("general-store");
    let config = crate::RuntimeConfig {
        provider_name: "ollama".into(),
        tools: Some(vec!["todo_write".into()]),
        prompt: crate::PromptTemplate::General,
        ..crate::RuntimeConfig::defaults(temp_dir("general-project"))
    };
    let agent = Agent::create(
        &config,
        SessionStore::with_root(store_dir.clone()),
        Arc::new(EventBus::default()),
    )
    .unwrap();
    assert_eq!(agent.registry_snapshot().names(), vec!["todo_write"]);
    let message = agent
        .invoke_tool("execute", json!({"command": "echo hi"}))
        .await
        .unwrap();
    let (_, text, is_error) = tool_result_of(&message, 0);
    assert!(is_error);
    assert!(text.contains("no tool registered"), "{text}");
    std::fs::remove_dir_all(&store_dir).ok();
}

#[test]
fn mcp_tools_replace_their_servers_previous_set_and_respect_the_allow_set() {
    let registry = Arc::new(std::sync::RwLock::new(Arc::new(ToolRegistry::new())));
    Arc::make_mut(&mut registry.write().unwrap()).register("read_file", Arc::new(EchoExecutor));
    let sink = RegistrySink {
        registry: registry.clone(),
        allowed: Some(vec![
            "read_file".into(),
            "gh__*".into(),
            "docs__search".into(),
        ]),
    };
    let tool = |name: &str| {
        (
            name.to_string(),
            Arc::new(EchoExecutor) as Arc<dyn ToolExecutor>,
        )
    };

    use arbe_mcp::ToolSink;
    sink.replace_server_tools("gh", vec![tool("gh__issues"), tool("gh__prs")]);
    sink.replace_server_tools("docs", vec![tool("docs__search"), tool("docs__delete")]);
    let names = |r: &Arc<std::sync::RwLock<Arc<ToolRegistry>>>| r.read().unwrap().names();
    assert_eq!(
        names(&registry),
        vec!["docs__search", "gh__issues", "gh__prs", "read_file"]
    );

    // A refreshed list replaces only that server's tools.
    sink.replace_server_tools("gh", vec![tool("gh__issues")]);
    assert_eq!(
        names(&registry),
        vec!["docs__search", "gh__issues", "read_file"]
    );
}

/// Full path through the harness: config → `Agent::create` → background
/// MCP connection → `McpServerConnected` → tool in the registry → call via
/// the approval gate. Uses the official reference server via `npx`.
/// Run with: `cargo test -p arbe-runtime reference_mcp -- --ignored`
#[tokio::test]
#[ignore = "needs Node/npx and network"]
async fn reference_mcp_server_tools_are_usable_through_the_agent() {
    let store_dir = temp_dir("mcp-store");
    let mut settings = arbe_mcp::McpServerSettings {
        command: Some("npx".into()),
        args: vec![
            "-y".into(),
            "@modelcontextprotocol/server-everything".into(),
        ],
        ..Default::default()
    };
    settings.timeout_secs = Some(120);
    let config = crate::RuntimeConfig {
        provider_name: "ollama".into(),
        mcp_servers: vec![settings.resolve("everything", &|_| None).unwrap()],
        policy_mode: ApprovalPolicyMode::AllowlistAuto,
        allowlist: vec!["everything__echo".into()],
        ..crate::RuntimeConfig::defaults(temp_dir("mcp-project"))
    };
    let events = Arc::new(EventBus::new(1_024));
    let mut rx = events.subscribe();
    let agent = Agent::create(&config, SessionStore::with_root(store_dir.clone()), events).unwrap();

    let tools = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match rx.recv().await.unwrap().event {
                RuntimeEvent::McpServerConnected { tools, .. } => return tools,
                RuntimeEvent::McpServerFailed { reason, .. } => panic!("{reason}"),
                _ => {}
            }
        }
    })
    .await
    .expect("server never connected");
    assert!(tools > 0);
    assert!(agent.registry_snapshot().contains("everything__echo"));

    let message = agent
        .invoke_tool("everything__echo", json!({"message": "through the agent"}))
        .await
        .unwrap();
    let (_, text, is_error) = tool_result_of(&message, 0);
    assert!(!is_error, "{text}");
    assert!(text.contains("through the agent"));
    std::fs::remove_dir_all(&store_dir).ok();
}

#[tokio::test]
async fn project_skills_are_indexed_on_demand_or_inlined_always() {
    let project = temp_dir("skills-project");
    let skill_dir = project.join(".arbe").join("skills");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("deploy.md"),
        "---\nname: arbe-test-deploy\ndescription: How to ship\n---\nRun make release.",
    )
    .unwrap();
    let store_dir = temp_dir("skills-store");
    let make = |mode: crate::SkillsMode| {
        let config = crate::RuntimeConfig {
            provider_name: "ollama".into(),
            skills_mode: mode,
            ..crate::RuntimeConfig::defaults(project.clone())
        };
        Agent::create(
            &config,
            SessionStore::with_root(store_dir.clone()),
            Arc::new(EventBus::default()),
        )
        .unwrap()
    };

    let on_demand = make(crate::SkillsMode::OnDemand);
    let instructions = on_demand.state().pipeline.skill_instructions.join("\n");
    assert!(instructions.contains("- arbe-test-deploy: How to ship"));
    assert!(!instructions.contains("Run make release."));
    // (The tool's behavior is covered in `skills.rs`.)
    assert!(on_demand.registry_snapshot().contains("load_skill"));

    let always = make(crate::SkillsMode::Always);
    let instructions = always.state().pipeline.skill_instructions.join("\n");
    assert!(instructions.contains("Run make release."));
    assert!(!always.registry_snapshot().contains("load_skill"));

    std::fs::remove_dir_all(&project).ok();
    std::fs::remove_dir_all(&store_dir).ok();
}

#[tokio::test]
async fn command_hooks_can_veto_tool_calls_and_their_failures_are_reported() {
    let veto = if cfg!(windows) {
        r#"echo {"veto": "blocked by policy script"}"#.to_string()
    } else {
        r#"echo '{"veto": "blocked by policy script"}'"#.to_string()
    };
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({"x": 1}))])],
        answer("ok"),
    );
    let requests = provider.requests.clone();
    let (t, mut rx) = test_agent_with(provider, |parts| {
        allow(&["echo"])(parts);
        parts.hooks.register(Arc::new(arbe_hooks::CommandHook::new(
            HookPhase::BeforeToolExecute,
            veto,
        )));
        parts.hooks.register(Arc::new(arbe_hooks::CommandHook::new(
            HookPhase::OnTurnComplete,
            "exit 4",
        )));
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));

    t.agent.submit_message("go".into()).await.unwrap();

    let requests = requests.lock().unwrap();
    let (_, text, is_error) = tool_result_of(requests[1].messages.last().unwrap(), 0);
    assert!(is_error);
    assert_eq!(text, "blocked by hook: blocked by policy script");
    let failed = drain(&mut rx).into_iter().find_map(|e| match e {
        RuntimeEvent::HookFailed { hook, reason } => Some((hook, reason)),
        _ => None,
    });
    let (hook, reason) = failed.expect("the failing hook is reported");
    assert!(
        hook.contains("on_turn_complete") && hook.contains("exit 4"),
        "{hook}"
    );
    assert!(reason.contains('4'), "{reason}");
}

#[tokio::test]
async fn secrets_in_tool_output_never_reach_the_model_the_log_or_the_screen() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[(
            "c1",
            "echo",
            json!({"leak": "sk-live-abcdef123456"}),
        )])],
        answer("done"),
    );
    let requests = provider.requests.clone();
    let (t, mut rx) = test_agent_with(provider, |parts| {
        allow(&["echo"])(parts);
        parts.redactor =
            crate::redact::Redactor::new(["sk-live-abcdef123456".to_string()], Vec::new());
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));
    t.agent.submit_message("go".into()).await.unwrap();

    // What the model saw.
    let requests = requests.lock().unwrap();
    let (_, sent, _) = tool_result_of(requests[1].messages.last().unwrap(), 0);
    assert!(!sent.contains("sk-live-abcdef123456"), "{sent}");
    assert!(sent.contains("[REDACTED]"));
    // What was persisted.
    let turns = t.store.list_turns(t.session_id).unwrap();
    let persisted = serde_json::to_string(&turns[0].messages[2]).unwrap();
    assert!(!persisted.contains("sk-live-abcdef123456"));
    // What the UI was told.
    for event in drain(&mut rx) {
        if let RuntimeEvent::ToolExecuted { result, .. } = event {
            assert!(!result.output.to_string().contains("sk-live-abcdef123456"));
        }
    }
}

#[test]
fn only_credential_looking_configured_values_are_redacted() {
    let config = crate::RuntimeConfig {
        api_key: Some("sk-config-key-000111".into()),
        extra_headers: vec![
            ("X-Api-Key".into(), "gateway-secret-999".into()),
            ("HTTP-Referer".into(), "https://example.dev".into()),
        ],
        ..crate::RuntimeConfig::defaults(temp_dir("redact"))
    };
    let r = redactor_for(&config);
    assert_eq!(
        r.redact("sk-config-key-000111 gateway-secret-999 https://example.dev"),
        "[REDACTED] [REDACTED] https://example.dev"
    );
}

#[tokio::test]
async fn a_long_tool_loop_prunes_its_own_old_output_but_persists_it_in_full() {
    let big = "y".repeat(20_000); // ~5000 tokens per result
    let rounds = (0..3)
        // Different arguments each round, or the repeated-call guard
        // (rightly) stops the loop.
        .map(|i| tool_calls(&[(&format!("c{i}"), "echo", json!(format!("{i}{big}")))]))
        .collect();
    let provider = ScriptedProvider::new(rounds, answer("done"));
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, |parts| {
        allow(&["echo"])(parts);
        parts.settings.budget_tokens = 8_000;
        parts.settings.max_tool_output_chars = 100_000;
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));
    t.agent.submit_message("go".into()).await.unwrap();

    let requests = requests.lock().unwrap();
    let last = &requests.last().unwrap().messages;
    let results: Vec<String> = last
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| tool_result_of(m, 0).1)
        .collect();
    assert_eq!(results.len(), 3);
    assert!(
        results[0].starts_with("[tool output omitted"),
        "oldest was pruned"
    );
    assert!(results[2].len() >= 20_000, "newest was kept");

    // The persisted trace has every result in full.
    let turns = t.store.list_turns(t.session_id).unwrap();
    let persisted: Vec<String> = turns[0]
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(|m| tool_result_of(m, 0).1)
        .collect();
    assert!(persisted.iter().all(|r| r.len() >= 20_000));
}

/// A plain answer with no usage report (so calibration stays neutral).
fn bare_answer(text: &str) -> Round {
    Round::Events(vec![
        ProviderEvent::TextDelta(text.to_string()),
        ProviderEvent::Stop(StopReason::EndTurn),
    ])
}

#[tokio::test]
async fn history_past_the_trigger_is_summarized_by_the_model_and_the_summary_replaces_it() {
    let long = "z".repeat(2_000); // ~500 tokens per answer
    let mut rounds: Vec<Round> = (0..4).map(|_| bare_answer(&long)).collect();
    rounds.push(bare_answer("- the user asked four questions about z")); // the summary
    rounds.push(bare_answer("final answer"));
    let provider = ScriptedProvider::new(rounds, bare_answer("unused"));
    let requests = provider.requests.clone();
    let (t, mut rx) = test_agent_with(provider, |parts| {
        parts.settings.auto_compact = true;
        parts.settings.budget_tokens = 2_000; // compacts past ~1600
    });
    for i in 0..4 {
        t.agent
            .submit_message(format!("question {i}"))
            .await
            .unwrap();
    }
    assert!(t.store.latest_compaction(t.session_id).unwrap().is_none());

    assert_eq!(
        t.agent.submit_message("question 4".into()).await.unwrap(),
        "final answer"
    );

    let compaction = t.store.latest_compaction(t.session_id).unwrap().unwrap();
    assert_eq!(
        compaction.summary,
        "- the user asked four questions about z"
    );
    assert!(compaction.through_turn_index >= 2);
    // Only the turns after the compaction remain in live history.
    assert!(
        t.agent
            .state()
            .history
            .iter()
            .all(|e| e.turn_index > compaction.through_turn_index)
    );

    let requests = requests.lock().unwrap();
    // The summarization request saw the old conversation, without tools.
    let summarize = &requests[4];
    assert!(summarize.tools.is_empty());
    assert!(summarize.messages[1].text().contains("User: question 0"));
    // The answer's request carries the summary instead of those turns.
    let answer = &requests[5].messages;
    assert!(
        answer.iter().any(|m| m.role == Role::System
            && m.text().contains("the user asked four questions about z"))
    );
    assert!(!answer.iter().any(|m| m.text() == "question 0"));

    assert!(drain(&mut rx)
        .iter()
        .any(|e| matches!(e, RuntimeEvent::CompactionPerformed { compacted_messages, .. } if *compacted_messages > 0)));
    // Every turn is still on disk.
    assert_eq!(t.store.list_turns(t.session_id).unwrap().len(), 5);
}

#[tokio::test]
async fn compact_now_summarizes_everything_but_the_newest_turn() {
    let rounds = vec![
        bare_answer("one"),
        bare_answer("two"),
        bare_answer("- summary of one"),
    ];
    let t = test_agent(ScriptedProvider::new(rounds, bare_answer("unused")));
    t.agent.submit_message("first".into()).await.unwrap();
    t.agent.submit_message("second".into()).await.unwrap();

    assert!(t.agent.compact().await.unwrap());
    let compaction = t.store.latest_compaction(t.session_id).unwrap().unwrap();
    assert_eq!(compaction.through_turn_index, 0);
    assert_eq!(
        t.agent.state().summary.as_ref().unwrap().summary,
        "- summary of one"
    );
    // With a single turn left there's nothing more to compact.
    assert!(!t.agent.compact().await.unwrap());
}

#[tokio::test]
async fn a_subdirectorys_instructions_arrive_with_the_first_tool_call_that_touches_it() {
    let project = temp_dir("nested-project");
    std::fs::create_dir_all(project.join("api")).unwrap();
    std::fs::write(
        project.join("api").join("AGENTS.md"),
        "Use snake_case in the API.",
    )
    .unwrap();
    std::fs::write(project.join("api").join("lib.rs"), "pub fn x() {}").unwrap();

    let provider = ScriptedProvider::new(
        vec![
            tool_calls(&[("c1", "read_file", json!({"path": "api/lib.rs"}))]),
            tool_calls(&[(
                "c2",
                "read_file",
                json!({"path": "api/lib.rs", "start_line": 1}),
            )]),
        ],
        answer("done"),
    );
    let requests = provider.requests.clone();
    let dir = project.clone();
    let (t, _rx) = test_agent_with(provider, move |parts| {
        allow(&["read_file"])(parts);
        parts.settings.project_dir = dir.clone();
        arbe_tools::builtin::register_all(&mut parts.registry, &dir);
    });
    t.agent
        .submit_message("look at the api".into())
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    let first = &requests[1].messages.last().unwrap().content[0];
    let ContentBlock::ToolResult { content, .. } = first else {
        panic!("expected a tool result");
    };
    assert_eq!(content.len(), 2);
    assert!(
        Message::with_blocks(Role::Tool, content.clone())
            .text()
            .contains("Use snake_case in the API.")
    );
    // The second touch doesn't repeat them.
    let second = &requests[2].messages.last().unwrap().content[0];
    let ContentBlock::ToolResult { content, .. } = second else {
        panic!("expected a tool result");
    };
    assert_eq!(content.len(), 1);
    std::fs::remove_dir_all(&project).ok();
}

/// Every tool call in `messages` is answered by a result in the tool
/// message right after it, and no result appears without its call —
/// what every provider requires of a request.
fn assert_tool_pairs_intact(messages: &[Message]) {
    for (i, message) in messages.iter().enumerate() {
        let calls = message.tool_uses();
        if calls.is_empty() {
            continue;
        }
        let next = messages
            .get(i + 1)
            .expect("tool calls at the very end of a request");
        assert_eq!(next.role, Role::Tool, "tool calls not followed by results");
        for call in calls {
            assert!(
                next.content.iter().any(|b| matches!(
                    b,
                    ContentBlock::ToolResult { tool_use_id, .. } if *tool_use_id == call.id
                )),
                "call {} has no result",
                call.id
            );
        }
    }
    for (i, message) in messages.iter().enumerate() {
        if message.role == Role::Tool {
            assert!(
                i > 0 && messages[i - 1].has_tool_uses(),
                "tool results without the calls before them"
            );
        }
    }
}

/// P5 exit criterion: 200 turns, each reading a large "file", against a
/// small budget. Every request must fit and keep call/result pairs whole.
#[tokio::test]
async fn a_200_turn_session_with_large_tool_output_stays_within_budget() {
    const BUDGET: u64 = 6_000;

    /// Small arguments, ~3000 tokens of output — like reading a big file.
    struct BigFile;
    #[async_trait]
    impl ToolExecutor for BigFile {
        async fn execute(
            &self,
            invocation: ToolInvocation,
            _ctx: &ToolContext,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult {
                id: invocation.id,
                output: json!("q".repeat(12_000)),
                is_error: false,
            })
        }
    }

    let mut rounds = Vec::new();
    for i in 0..200 {
        rounds.push(Round::Events(vec![
            ProviderEvent::ToolUseStart {
                id: format!("c{i}"),
                name: "big_file".into(),
            },
            ProviderEvent::ToolUseInputDelta {
                id: format!("c{i}"),
                partial_json: json!({ "path": format!("file{i}.txt") }).to_string(),
            },
            ProviderEvent::Stop(StopReason::ToolUse),
        ]));
        rounds.push(bare_answer(&format!("answer {i}")));
    }
    let provider = ScriptedProvider::new(rounds, bare_answer("unused"));
    let requests = provider.requests.clone();
    let (t, _rx) = test_agent_with(provider, |parts| {
        allow(&["big_file"])(parts);
        parts.settings.budget_tokens = BUDGET;
        parts.settings.max_tool_output_chars = 100_000;
    });
    t.agent.register_tool("big_file", Arc::new(BigFile));

    for i in 0..200 {
        t.agent
            .submit_message(format!("question {i}"))
            .await
            .unwrap();
    }

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 400);
    for (n, request) in requests.iter().enumerate() {
        let tokens: u64 = request
            .messages
            .iter()
            .map(arbe_memory::estimate_message_tokens)
            .sum();
        assert!(tokens <= BUDGET, "request {n} is {tokens} tokens");
        assert_tool_pairs_intact(&request.messages);
    }
    assert_eq!(t.store.list_turns(t.session_id).unwrap().len(), 200);
}

/// P5 exit criterion: a compaction is persisted and survives resume.
#[tokio::test]
async fn a_compaction_survives_resume() {
    let rounds = vec![
        bare_answer("one"),
        bare_answer("two"),
        bare_answer("three"),
        bare_answer("- summary of one and two"),
    ];
    let t = test_agent(ScriptedProvider::new(rounds, bare_answer("unused")));
    for q in ["first", "second", "third"] {
        t.agent.submit_message(q.into()).await.unwrap();
    }
    assert!(t.agent.compact().await.unwrap());

    // A new process resuming the same session.
    let config = crate::RuntimeConfig {
        provider_name: "ollama".into(),
        ..crate::RuntimeConfig::defaults(temp_dir("resume-project"))
    };
    let resumed = Agent::resume(
        &config,
        t.store.clone(),
        t.session_id,
        Arc::new(EventBus::default()),
    )
    .unwrap();
    let state = resumed.state();
    assert_eq!(
        state.summary.as_ref().unwrap().summary,
        "- summary of one and two"
    );
    // Only the newest turn remains as live history; the rest is the summary.
    assert!(state.history.iter().all(|e| e.turn_index == 2));
    assert_eq!(state.next_turn_index, 3);
}

// ---------------------------------------------------------------------------
// Session metadata other programs read (workdir, branch, activity, title)
// ---------------------------------------------------------------------------

#[test]
fn a_new_session_records_where_it_works_and_that_it_is_open() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    std::fs::write(project.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut config = RuntimeConfig::defaults(project.path().to_path_buf());
    config.home = home.path().to_path_buf();
    let store = SessionStore::with_root(home.path().join("sessions"));

    let agent = Agent::create(&config, store.clone(), Arc::new(EventBus::new(16))).unwrap();
    let meta = store.load_meta(agent.session_id()).unwrap();
    assert_eq!(meta.workdir.as_deref(), Some(project.path()));
    assert_eq!(meta.branch.as_deref(), Some("main"));
    assert_eq!(meta.pid, Some(std::process::id()));
    assert_eq!(meta.activity, Some(arbe_core::SessionActivity::Idle));
    assert_eq!(meta.title, None);

    agent.set_title("conflict fix").unwrap();
    agent.close().unwrap();
    let meta = store.load_meta(agent.session_id()).unwrap();
    assert_eq!(meta.title.as_deref(), Some("conflict fix"));
    assert_eq!((meta.activity, meta.pid), (None, None));
    assert_eq!(meta.status, SessionStatus::Closed);

    // Resuming reopens it.
    let resumed =
        Agent::resume(&config, store.clone(), meta.id, Arc::new(EventBus::new(16))).unwrap();
    let meta = store.load_meta(resumed.session_id()).unwrap();
    assert_eq!(meta.activity, Some(arbe_core::SessionActivity::Idle));
    assert_eq!(meta.pid, Some(std::process::id()));
}

struct RecordHook {
    phase: HookPhase,
    seen: Arc<Mutex<Vec<Value>>>,
}

#[async_trait]
impl Hook for RecordHook {
    fn phase(&self) -> HookPhase {
        self.phase
    }
    async fn run(&self, payload: Value) -> Result<Value, HookError> {
        self.seen.lock().unwrap().push(payload.clone());
        Ok(payload)
    }
}

#[tokio::test]
async fn activity_and_title_are_kept_current_and_approval_waits_fire_a_hook() {
    let provider = ScriptedProvider::new(
        vec![tool_calls(&[("c1", "echo", json!({"x": 1}))])],
        answer("final answer"),
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (t, mut rx) = test_agent_with(provider, |parts| {
        for phase in [HookPhase::OnApprovalRequested, HookPhase::OnTurnComplete] {
            parts.hooks.register(Arc::new(RecordHook {
                phase,
                seen: seen.clone(),
            }));
        }
    });
    t.agent.register_tool("echo", Arc::new(EchoExecutor));
    let meta = || t.store.load_meta(t.session_id).unwrap();

    let agent = t.agent.clone();
    let turn =
        tokio::spawn(async move { agent.submit_message("\n  use echo \nplease".into()).await });
    let id = wait_for(&mut rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    assert_eq!(
        meta().activity,
        Some(arbe_core::SessionActivity::AwaitingApproval)
    );
    assert_eq!(meta().title.as_deref(), Some("use echo"));

    t.agent
        .supply_tool_decision(id, ApprovalDecision::ApprovedOnce);
    turn.await.unwrap().unwrap();
    assert_eq!(meta().activity, Some(arbe_core::SessionActivity::Idle));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let session_id = t.session_id.to_string();
    assert_eq!(seen[0]["tool_name"], "echo");
    assert_eq!(seen[0]["arguments"], json!({"x": 1}));
    assert_eq!(seen[0]["tool_call_id"], id.to_string());
    // Every payload carries the session id.
    assert!(seen.iter().all(|p| p["session_id"] == session_id.as_str()));
}

#[test]
fn titles_come_from_the_first_non_empty_line_shortened() {
    assert_eq!(
        title_from("\n\n  fix the build \nsecond"),
        Some("fix the build".into())
    );
    assert_eq!(title_from("   \n"), None);
    let long = title_from(&"word ".repeat(40)).unwrap();
    assert!(long.ends_with('…') && long.chars().count() <= 60);
}

// ---------------------------------------------------------------------------
// Subagents (the `task` tool)
// ---------------------------------------------------------------------------

/// Plays both sides: the parent (asked `PARENT-Q`) delegates with `task`;
/// the child (asked `CHILD-TASK ...`) reads `notes.txt` and reports.
/// Every request is recorded, so tests can see what each side was sent.
#[derive(Clone, Default)]
struct DelegatingModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl DelegatingModel {
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn mentions(message: &Message, needle: &str) -> bool {
    serde_json::to_string(&message.content)
        .unwrap()
        .contains(needle)
}

#[async_trait]
impl ModelProvider for DelegatingModel {
    fn id(&self) -> &str {
        "delegating"
    }
    fn capabilities(&self, _model: &str) -> ModelCapabilities {
        ModelCapabilities {
            streaming: true,
            tool_calls: true,
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
        self.requests.lock().unwrap().push(req.clone());
        let is_parent = req
            .messages
            .iter()
            .any(|m| m.role == Role::User && mentions(m, "PARENT-Q"));
        let last = req.messages.last().unwrap();
        let round = match (is_parent, last.role == Role::Tool) {
            (true, false) => tool_calls(&[(
                "t1",
                "task",
                json!({"description": "look up the code", "prompt": "CHILD-TASK: read notes.txt and report the code"}),
            )]),
            (false, false) => tool_calls(&[("r1", "read_file", json!({"path": "notes.txt"}))]),
            (false, true) => answer(if mentions(last, "secret-42") {
                "the code is secret-42"
            } else {
                "could not read it"
            }),
            (true, true) => answer(if mentions(last, "the code is secret-42") {
                "my subagent says: secret-42"
            } else {
                "the subagent failed"
            }),
        };
        let Round::Events(events) = round else {
            unreachable!()
        };
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}

struct Tree {
    agent: Arc<Agent>,
    rx: tokio::sync::broadcast::Receiver<arbe_core::EventEnvelope>,
    model: DelegatingModel,
    store: SessionStore,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn delegating_agent() -> Tree {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("notes.txt"), "code: secret-42").unwrap();
    let model = DelegatingModel::default();
    let mut providers = ProviderRegistry::new();
    let shared = model.clone();
    providers.register("delegating", move |_| {
        Ok(Box::new(shared.clone()) as Box<dyn ModelProvider>)
    });
    let config = crate::RuntimeConfig {
        provider_name: "delegating".into(),
        home: home.path().to_path_buf(),
        // Default approval mode: every call asks, the child's included.
        ..crate::RuntimeConfig::defaults(project.path().to_path_buf())
    };
    let store = SessionStore::with_root(home.path().join("sessions"));
    let events = Arc::new(EventBus::new(4_096));
    let rx = events.subscribe();
    let agent = Arc::new(Agent::create_with(&config, store.clone(), events, &providers).unwrap());
    Tree {
        agent,
        rx,
        model,
        store,
        _dirs: (home, project),
    }
}

#[tokio::test]
async fn a_subagent_works_in_its_own_context_and_its_approvals_reach_the_parent() {
    let mut tree = delegating_agent();
    let agent = tree.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("PARENT-Q".into()).await });

    // First the parent's own `task` call asks for approval...
    let task_id = wait_for(&mut tree.rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    assert!(
        tree.agent
            .supply_tool_decision(task_id, ApprovalDecision::ApprovedOnce)
    );

    // ...then the child's `read_file`, wrapped as a subagent event and
    // answered through the parent.
    let (child_call, child_session) = wait_for(&mut tree.rx, |e| match e {
        RuntimeEvent::SubagentEvent {
            parent_tool_call_id,
            session_id,
            event,
        } => match *event {
            RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => {
                assert_eq!(parent_tool_call_id, task_id);
                Some((tool_call_id, session_id))
            }
            _ => None,
        },
        _ => None,
    })
    .await;
    assert!(
        tree.agent
            .supply_tool_decision(child_call, ApprovalDecision::ApprovedOnce)
    );

    let reply = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("turn never finished")
        .unwrap()
        .unwrap();
    assert_eq!(reply, "my subagent says: secret-42");

    // Context isolation: the child never saw the parent's conversation,
    // and at the depth cap it wasn't offered `task` itself.
    let requests = tree.model.requests();
    let child_requests: Vec<_> = requests
        .iter()
        .filter(|r| !r.messages.iter().any(|m| mentions(m, "PARENT-Q")))
        .collect();
    assert_eq!(child_requests.len(), 2);
    for request in &child_requests {
        assert!(!request.tools.iter().any(|t| t.name == "task"));
        assert!(request.tools.iter().any(|t| t.name == "read_file"));
    }
    assert!(requests[0].tools.iter().any(|t| t.name == "task"));
    // The parent saw the child's answer, not its tool traffic.
    let parent_last = requests.last().unwrap();
    assert!(
        !parent_last
            .messages
            .iter()
            .any(|m| mentions(m, "code: secret-42"))
    );

    // The child's session is saved, named, closed, and linked to its parent.
    let child = tree.store.load_meta(child_session).unwrap();
    assert_eq!(child.parent, Some(tree.agent.session_id()));
    assert_eq!(child.title.as_deref(), Some("look up the code"));
    assert_eq!(child.status, SessionStatus::Closed);
    assert_eq!(tree.store.list_turns(child_session).unwrap().len(), 1);
}

#[tokio::test]
async fn cancelling_the_parent_cancels_a_waiting_subagent() {
    let mut tree = delegating_agent();
    let agent = tree.agent.clone();
    let turn = tokio::spawn(async move { agent.submit_message("PARENT-Q".into()).await });
    let task_id = wait_for(&mut tree.rx, |e| match e {
        RuntimeEvent::ToolApprovalRequested { tool_call_id, .. } => Some(tool_call_id),
        _ => None,
    })
    .await;
    tree.agent
        .supply_tool_decision(task_id, ApprovalDecision::ApprovedOnce);
    // Wait until the child is paused on its own approval, then cancel.
    let child_session = wait_for(&mut tree.rx, |e| match e {
        RuntimeEvent::SubagentEvent {
            session_id, event, ..
        } if matches!(*event, RuntimeEvent::ToolApprovalRequested { .. }) => Some(session_id),
        _ => None,
    })
    .await;
    assert!(tree.agent.cancel_turn());

    let result = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("turn never finished")
        .unwrap();
    assert!(matches!(result, Err(HarnessError::Cancelled)), "{result:?}");
    let child_turns = tree.store.list_turns(child_session).unwrap();
    assert_eq!(child_turns[0].stop_reason, Some(StopReason::Cancelled));
    assert_eq!(
        tree.store.load_meta(child_session).unwrap().status,
        SessionStatus::Closed
    );
}
