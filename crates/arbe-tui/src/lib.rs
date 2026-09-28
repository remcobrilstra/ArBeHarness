//! Terminal UI (TUI spec). Depends only on `arbe-runtime`'s event/command
//! contract (reached via its re-exports of `arbe_core`/`arbe_tools`), and
//! contains no agent decision logic (TUI spec §2) — everything here is
//! either presentation state (`app.rs`) or rendering (`ui.rs`).

pub mod app;
pub mod markdown;
pub mod ui;

pub use arbe_runtime;

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use arbe_runtime::arbe_core::{
    ApprovalDecision, EventEnvelope, Role, RuntimeEvent, SessionId, ToolCallId, ToolResult,
};
use arbe_runtime::arbe_storage::SessionStore;
use arbe_runtime::arbe_tools::ToolExecutor;
use arbe_runtime::{Agent, EventBus, RuntimeConfig, ToolDecisions};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;

use app::{APPROVAL_TIMEOUT_TICKS, App, PendingApproval, ProposedToolCall, SessionPicker};

/// Result of a spawned agent call, delivered back to the render loop so it
/// never has to block on the async work itself. Tool-call *results*
/// aren't carried here — they arrive as `RuntimeEvent::ToolExecuted`/
/// `ToolCallDenied` (see `drain_runtime_events`), which covers both the
/// manual `/tool` path and a model-initiated one uniformly. This variant
/// exists only to surface a hard failure resolving the call itself (a
/// dropped lock, an unknown id).
enum AgentOutcome {
    ChatDone(Result<String, String>),
    ToolResolved(ToolCallId, String, Result<(), String>),
}

/// Runs the TUI until the user quits. Blocking: call this from a dedicated
/// OS thread (e.g. `tokio::task::spawn_blocking`) — it drives its own
/// render/input loop and uses `handle` to run the agent's async methods
/// from that thread via `Handle::block_on`/`Handle::spawn`.
///
/// `config`/`events` are the same values the caller used to build `agent`
/// with `Agent::create` — the TUI needs them to start new sessions or
/// resume old ones (TUI-FR-3) without the caller having to reconstruct
/// them.
pub fn run(
    agent: Agent,
    handle: Handle,
    config: RuntimeConfig,
    events: Arc<EventBus>,
) -> io::Result<()> {
    let mut events_rx = agent.subscribe_events();
    let profile = agent.profile().to_string();
    let provider_name = agent.provider_name().to_string();
    let model = agent.model().to_string();
    let project_dir = agent.project_dir().display().to_string();
    let session_id = agent.session_id();
    // Held separately from `agent`'s lock — see `ToolDecisions`'s doc
    // comment in arbe-runtime for why supplying a decision must never
    // need to acquire the same lock a paused `submit_message` call holds.
    let tool_decisions = std::sync::Mutex::new(agent.tool_decisions());

    let agent = Arc::new(AsyncMutex::new(agent));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(session_id, profile, provider_name, model, project_dir);
    let (outcome_tx, outcome_rx) = channel::<AgentOutcome>();

    let result = event_loop(
        &mut terminal,
        &mut app,
        &agent,
        &tool_decisions,
        &handle,
        &config,
        &events,
        &mut events_rx,
        &outcome_tx,
        &outcome_rx,
    );

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Flush session state on exit (TUI-FR-3: "exit safely with state flush").
    if let Ok(mut guard) = agent.try_lock() {
        let _ = guard.close();
    }

    result
}

/// Registers a local tool so the `/tool` demo command has something real
/// to execute (see `app.rs`/README for how to add more).
pub fn register_demo_tool(
    agent: &mut Agent,
    name: impl Into<String>,
    executor: Arc<dyn ToolExecutor>,
) {
    agent.register_tool(name, executor);
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
    events_rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    outcome_tx: &Sender<AgentOutcome>,
    outcome_rx: &Receiver<AgentOutcome>,
) -> io::Result<()> {
    loop {
        drain_runtime_events(app, events_rx);
        drain_outcomes(app, outcome_rx);
        tick_approval_timeout(app, agent, tool_decisions, handle, outcome_tx);

        terminal.draw(|f| ui::draw(f, app))?;

        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(80))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(
                key,
                app,
                agent,
                tool_decisions,
                handle,
                config,
                events,
                outcome_tx,
            );
        }
    }
}

fn drain_runtime_events(
    app: &mut App,
    events_rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
) {
    loop {
        match events_rx.try_recv().map(|envelope| envelope.event) {
            Ok(RuntimeEvent::ContextBuilt {
                estimated_tokens, ..
            }) => {
                app.last_estimated_tokens = estimated_tokens;
                app.activity = Some(format!(
                    "calling model (~{estimated_tokens} tokens context)…"
                ));
            }
            Ok(RuntimeEvent::ProviderRetrying {
                attempt,
                delay_ms,
                reason,
                ..
            }) => {
                let secs = delay_ms.div_ceil(1000);
                app.activity = Some(format!(
                    "{reason} — retrying in {secs}s (attempt {attempt})…"
                ));
            }
            Ok(RuntimeEvent::ModelStreamChunk { delta, .. }) => {
                app.activity = None;
                app.append_assistant_delta(&delta);
            }
            Ok(RuntimeEvent::TurnCompleted { .. }) => {
                app.working = false;
                app.activity = None;
            }
            Ok(RuntimeEvent::RuntimeError { reason, .. }) => {
                app.working = false;
                app.activity = None;
                app.status_message = Some(reason);
            }
            Ok(RuntimeEvent::ToolCallProposed {
                turn_id,
                tool_call_id,
                tool_name,
                arguments,
                risk,
            }) => {
                let arguments_pretty = serde_json::to_string(&arguments).unwrap_or_default();
                app.activity = Some(format!("running tool: {tool_name}…"));
                app.push_line(
                    Role::Tool,
                    format!("→ {tool_name} {arguments_pretty} (risk: {risk:?})"),
                );
                app.proposed_tool_calls.insert(
                    tool_call_id,
                    ProposedToolCall {
                        tool_name,
                        arguments_pretty,
                        risk,
                        source_turn: turn_id,
                    },
                );
            }
            Ok(RuntimeEvent::ToolApprovalRequested { tool_call_id, .. }) => {
                if let Some(meta) = app.proposed_tool_calls.remove(&tool_call_id) {
                    app.pending_approval = Some(PendingApproval {
                        id: tool_call_id,
                        tool_name: meta.tool_name,
                        arguments_pretty: meta.arguments_pretty,
                        risk: meta.risk,
                        source_turn: meta.source_turn,
                        ticks_remaining: APPROVAL_TIMEOUT_TICKS,
                    });
                }
            }
            Ok(RuntimeEvent::ToolExecuted {
                tool_name, result, ..
            }) => {
                app.activity = Some("continuing…".to_string());
                app.push_line(Role::Tool, format_tool_result(&tool_name, &result));
            }
            Ok(RuntimeEvent::ToolCallDenied {
                tool_name, reason, ..
            }) => {
                app.activity = Some("continuing…".to_string());
                app.push_line(Role::Tool, format!("✗ {tool_name} denied: {reason}"));
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                // The event bus's broadcast channel has a fixed capacity;
                // if this receiver falls behind (e.g. a busy redraw/resize)
                // during a verbose streamed response, the oldest unread
                // events are dropped rather than buffered forever. Silently
                // continuing here would leave the transcript looking
                // subtly wrong (a partial/garbled response) with no
                // indication anything was lost — surface it instead.
                app.status_message = Some(format!(
                    "warning: missed {skipped} runtime event(s) (fell behind); \
                     transcript may be incomplete"
                ));
                continue;
            }
        }
    }
}

/// Caps how much of a tool's output gets echoed into the transcript — a
/// broad `grep`/`glob`/`read_file` call can return a lot of text, and the
/// point here is visibility into what the agent did, not a full result
/// dump (that's still available by inspecting the tool call itself).
const TOOL_OUTPUT_PREVIEW_CHARS: usize = 500;

fn format_tool_result(tool_name: &str, result: &ToolResult) -> String {
    let output = serde_json::to_string(&result.output).unwrap_or_default();
    let truncated = if output.chars().count() > TOOL_OUTPUT_PREVIEW_CHARS {
        let head: String = output.chars().take(TOOL_OUTPUT_PREVIEW_CHARS).collect();
        format!("{head}… (truncated)")
    } else {
        output
    };
    if result.is_error {
        format!("✗ {tool_name} failed: {truncated}")
    } else {
        format!("✓ {tool_name} → {truncated}")
    }
}

fn drain_outcomes(app: &mut App, outcome_rx: &Receiver<AgentOutcome>) {
    while let Ok(outcome) = outcome_rx.try_recv() {
        match outcome {
            AgentOutcome::ChatDone(Err(err)) => {
                app.working = false;
                app.activity = None;
                app.status_message = Some(err);
            }
            AgentOutcome::ChatDone(Ok(_)) => {}
            AgentOutcome::ToolResolved(id, tool_name, Err(err)) => {
                app.status_message = Some(format!("tool call {id} ({tool_name}) failed: {err}"))
            }
            AgentOutcome::ToolResolved(_, _, Ok(())) => {}
        }
    }
}

/// Resolves a pending approval with `decision`, dispatching to whichever
/// of the two pending-approval mechanisms actually has `id`:
/// - A model-initiated call paused inside `Agent::run_tool_loop` — supplied
///   via `ToolDecisions`, which needs no `Agent` lock at all (see its doc
///   comment for why that matters: `run_tool_loop` is holding that lock
///   for the whole turn, including this wait).
/// - The manual `/tool` demo path (`Agent::propose_tool_call`) — falls
///   back to `resolve_tool_call`, which *does* need the lock, briefly,
///   since it calls `execute_gated` itself rather than unblocking an
///   already-in-flight call.
fn resolve_approval(
    id: ToolCallId,
    tool_name: String,
    decision: ApprovalDecision,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    outcome_tx: &Sender<AgentOutcome>,
) {
    let supplied = tool_decisions
        .lock()
        .expect("tool decisions mutex poisoned")
        .supply(id, decision);
    if supplied {
        return;
    }

    let agent = agent.clone();
    let tx = outcome_tx.clone();
    handle.spawn(async move {
        let mut a = agent.lock().await;
        let result = a
            .resolve_tool_call(id, decision)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
        let _ = tx.send(AgentOutcome::ToolResolved(id, tool_name, result));
    });
}

/// Decrements the pending approval's countdown once per render-loop tick
/// (~80ms — see `event_loop`'s `event::poll` timeout) and auto-denies it
/// on expiry, per TUI spec §9 ("if approval prompt times out: default
/// action from policy, recommended deny... show user what action was
/// applied").
fn tick_approval_timeout(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    outcome_tx: &Sender<AgentOutcome>,
) {
    let Some(approval) = app.pending_approval.as_mut() else {
        return;
    };
    if approval.ticks_remaining > 0 {
        approval.ticks_remaining -= 1;
        return;
    }
    let id = approval.id;
    let tool_name = approval.tool_name.clone();
    app.pending_approval = None;
    app.status_message = Some(format!(
        "tool call {id} ({tool_name}) timed out — auto-denied"
    ));
    resolve_approval(
        id,
        tool_name,
        ApprovalDecision::DeniedOnce,
        agent,
        tool_decisions,
        handle,
        outcome_tx,
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
    outcome_tx: &Sender<AgentOutcome>,
) {
    if let Some(approval) = app.pending_approval.clone() {
        let decision = match key.code {
            KeyCode::Char('y') => Some(ApprovalDecision::ApprovedOnce),
            KeyCode::Char('n') => Some(ApprovalDecision::DeniedOnce),
            KeyCode::Char('a') => Some(ApprovalDecision::ApprovedForSession),
            KeyCode::Char('d') => Some(ApprovalDecision::AlwaysDeniedForSession),
            _ => None,
        };
        if let Some(decision) = decision {
            app.pending_approval = None;
            resolve_approval(
                approval.id,
                approval.tool_name.clone(),
                decision,
                agent,
                tool_decisions,
                handle,
                outcome_tx,
            );
        }
        return;
    }

    if app.session_picker.is_some() {
        match key.code {
            KeyCode::Up => {
                if let Some(picker) = app.session_picker.as_mut() {
                    picker.move_up();
                }
            }
            KeyCode::Down => {
                if let Some(picker) = app.session_picker.as_mut() {
                    picker.move_down();
                }
            }
            KeyCode::Esc => app.session_picker = None,
            KeyCode::Enter => {
                let selected = app
                    .session_picker
                    .as_ref()
                    .and_then(|p| p.selected_session());
                if let Some(session_id) = selected {
                    resume_session(
                        app,
                        agent,
                        tool_decisions,
                        handle,
                        config,
                        events,
                        session_id,
                    );
                } else {
                    app.session_picker = None;
                }
            }
            _ => {}
        }
        return;
    }

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => app.should_quit = true,
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
            app.clear_transcript();
            app.scroll = 0;
            app.follow_tail = true;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('n')) => {
            new_session(app, agent, tool_decisions, handle, config, events)
        }
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => open_session_picker(app),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            app.scroll_up((app.last_viewport_height / 2).max(1))
        }
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => {
            app.scroll_down((app.last_viewport_height / 2).max(1))
        }
        (_, KeyCode::PageUp) => app.scroll_up(app.last_viewport_height.max(1)),
        (_, KeyCode::PageDown) => app.scroll_down(app.last_viewport_height.max(1)),
        (_, KeyCode::Up) => app.scroll_up(1),
        (_, KeyCode::Down) => app.scroll_down(1),
        (KeyModifiers::ALT, KeyCode::Enter) | (KeyModifiers::SHIFT, KeyCode::Enter) => {
            app.input_insert_newline()
        }
        (_, KeyCode::Enter) => submit_input(app, agent, handle, outcome_tx),
        (_, KeyCode::Backspace) => app.input_backspace(),
        (_, KeyCode::Delete) => app.input_delete_forward(),
        (_, KeyCode::Left) => app.input_move_left(),
        (_, KeyCode::Right) => app.input_move_right(),
        (_, KeyCode::Home) => app.input_move_home(),
        (_, KeyCode::End) => app.input_move_end(),
        (_, KeyCode::Char(c)) => app.input_insert_char(c),
        _ => {}
    }
}

/// Replaces the running agent in place (new session / resume) and resets
/// the transcript view to match. `handle` for locking only — the caller
/// has already built `new_agent` against the same shared `EventBus`, so
/// the existing `events_rx` subscription in `event_loop` keeps working
/// without resubscribing. Also re-points `tool_decisions` at the new
/// agent's own mailbox — each `Agent` has its own `ToolDecisions`, so the
/// old handle would silently `supply()` into a mailbox nothing reads from
/// anymore.
fn swap_in_agent(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    new_agent: Agent,
) {
    app.session_id = new_agent.session_id();
    app.profile = new_agent.profile().to_string();
    app.provider_name = new_agent.provider_name().to_string();
    app.model = new_agent.model().to_string();
    app.clear_transcript();
    app.pending_approval = None;
    app.proposed_tool_calls.clear();
    app.status_message = None;
    app.notice = None;
    app.scroll = 0;
    app.follow_tail = true;
    *tool_decisions
        .lock()
        .expect("tool decisions mutex poisoned") = new_agent.tool_decisions();
    let mut guard = handle.block_on(agent.lock());
    *guard = new_agent;
}

fn new_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
) {
    match Agent::create(config, SessionStore::new(), events.clone()) {
        Ok(new_agent) => {
            swap_in_agent(app, agent, tool_decisions, handle, new_agent);
            app.notice = Some("started a new session".to_string());
        }
        Err(err) => app.status_message = Some(format!("failed to start new session: {err}")),
    }
}

fn open_session_picker(app: &mut App) {
    match SessionStore::new().list_sessions() {
        Ok(sessions) => app.session_picker = Some(SessionPicker::new(sessions)),
        Err(err) => app.status_message = Some(format!("failed to list sessions: {err}")),
    }
}

fn resume_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    tool_decisions: &std::sync::Mutex<ToolDecisions>,
    handle: &Handle,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
    session_id: SessionId,
) {
    let store = SessionStore::new();
    match Agent::resume(config, store.clone(), session_id, events.clone()) {
        Ok(new_agent) => {
            let turns = store.list_turns(session_id).unwrap_or_default();
            swap_in_agent(app, agent, tool_decisions, handle, new_agent);
            for turn in turns {
                if let Some(m) = turn.user_message() {
                    app.push_line(Role::User, m.text());
                }
                if let Some(m) = turn.final_assistant_message() {
                    app.push_line(Role::Assistant, m.text());
                }
            }
            app.notice = Some(format!("resumed session {session_id}"));
        }
        Err(err) => app.status_message = Some(format!("failed to resume session: {err}")),
    }
    app.session_picker = None;
}

fn submit_input(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    handle: &Handle,
    outcome_tx: &Sender<AgentOutcome>,
) {
    let content = app.take_input();
    if content.is_empty() {
        return;
    }
    app.status_message = None;
    app.notice = None;

    if let Some(rest) = content.strip_prefix("/tool ") {
        let Some((name, args_text)) = rest.split_once(' ') else {
            app.status_message = Some("usage: /tool <name> <json-args>".to_string());
            return;
        };
        let name = name.to_string();
        let arguments: serde_json::Value =
            serde_json::from_str(args_text).unwrap_or(serde_json::json!({}));

        let agent = agent.clone();
        // propose_tool_call itself is synchronous, but the agent lives
        // behind an async mutex shared with the render loop's other spawned
        // tasks (chat turns hold this lock for their entire streaming
        // duration) — block_on here would freeze the whole render loop
        // (no redraws, no input) until any in-flight turn finishes, so this
        // goes through handle.spawn like every other agent call instead.
        handle.spawn(async move {
            let mut a = agent.lock().await;
            a.propose_tool_call(name, arguments, arbe_runtime::arbe_core::RiskLevel::Medium);
        });
        // The resulting ToolCallProposed/ToolApprovalRequested events
        // arrive on the next drain_runtime_events and populate
        // app.pending_approval from there (see event_loop).
        return;
    }

    app.push_line(Role::User, content.clone());
    app.working = true;
    app.activity = Some("sending…".to_string());
    let agent = agent.clone();
    let tx = outcome_tx.clone();
    handle.spawn(async move {
        let mut a = agent.lock().await;
        let result = a.submit_message(content).await.map_err(|e| e.to_string());
        let _ = tx.send(AgentOutcome::ChatDone(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_runtime::arbe_core::TurnId;

    fn test_app() -> App {
        App::new(
            SessionId::new(),
            "default".to_string(),
            "fake".to_string(),
            "fake-model".to_string(),
            ".".to_string(),
        )
    }

    #[test]
    fn drain_runtime_events_applies_a_normal_event() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut app = test_app();
        let session_id = app.session_id;
        let turn_id = TurnId::new();

        tx.send(EventEnvelope {
            seq: 0,
            event: RuntimeEvent::TurnCompleted {
                session_id,
                turn_id,
            },
        })
        .unwrap();
        app.working = true;

        drain_runtime_events(&mut app, &mut rx);

        assert!(!app.working);
    }

    #[test]
    fn drain_runtime_events_surfaces_a_status_message_when_events_are_dropped() {
        // A tiny capacity forces the receiver to lag as soon as it falls
        // behind by more than a couple of sends.
        let (tx, mut rx) = tokio::sync::broadcast::channel(2);
        let mut app = test_app();
        let session_id = app.session_id;

        for _ in 0..10 {
            let _ = tx.send(EventEnvelope {
                seq: 0,
                event: RuntimeEvent::TurnCompleted {
                    session_id,
                    turn_id: TurnId::new(),
                },
            });
        }

        drain_runtime_events(&mut app, &mut rx);

        let message = app.status_message.expect("a lag warning should be set");
        assert!(
            message.contains("missed"),
            "expected a lag warning, got: {message}"
        );
    }
}
