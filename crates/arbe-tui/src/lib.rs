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
    ApprovalDecision, EventEnvelope, Role, RuntimeEvent, SessionId, StopReason, ToolCallId,
    ToolResult,
};
use arbe_runtime::arbe_storage::SessionStore;
use arbe_runtime::arbe_tools::ToolExecutor;
use arbe_runtime::{Agent, EventBus, RuntimeConfig};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::runtime::Handle;

use app::{APPROVAL_TIMEOUT_TICKS, App, PendingApproval, ProposedToolCall, SessionPicker};

/// Result of a spawned agent call, delivered back to the render loop so it
/// never has to block on the async work itself. Tool results and turn
/// progress arrive as `RuntimeEvent`s (see `drain_runtime_events`); these
/// only carry a hard failure of the call itself (e.g. `Busy`).
enum AgentOutcome {
    ChatDone(Result<String, String>),
    ToolInvoked(Result<(), String>),
}

/// Runs the TUI until the user quits. Blocking: call this from a dedicated
/// OS thread (e.g. `tokio::task::spawn_blocking`) — it drives its own
/// render/input loop and uses `handle` to spawn the agent's async methods.
///
/// The agent is shared as a plain `Arc` (all of its methods take `&self`),
/// so answering an approval or cancelling a turn works while a turn is
/// running. `config`/`events` are the values `agent` was built with; they
/// are needed to start new sessions or resume old ones (TUI-FR-3).
pub fn run(
    agent: Agent,
    handle: Handle,
    config: RuntimeConfig,
    events: Arc<EventBus>,
) -> io::Result<()> {
    let mut events_rx = agent.subscribe_events();
    let mut app = App::new(
        agent.session_id(),
        agent.profile().to_string(),
        agent.provider_name().to_string(),
        agent.model().to_string(),
        agent.project_dir().display().to_string(),
    );
    let mut agent = Arc::new(agent);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (outcome_tx, outcome_rx) = channel::<AgentOutcome>();

    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut agent,
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

    // Stop any in-flight turn and give it a moment to persist what it has
    // before the process exits (TUI-FR-3: "exit safely with state flush").
    // If it doesn't finish in time, the in-flight log still lets the next
    // resume recover it.
    if agent.cancel_turn() {
        for _ in 0..40 {
            if !agent.is_busy() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = agent.close();

    result
}

/// Registers a local tool so the `/tool` command has something to run.
pub fn register_demo_tool(agent: &Agent, name: impl Into<String>, executor: Arc<dyn ToolExecutor>) {
    agent.register_tool(name, executor);
}

#[allow(clippy::too_many_arguments)]
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    agent: &mut Arc<Agent>,
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
        tick_approval_timeout(app, agent);

        terminal.draw(|f| ui::draw(f, app))?;

        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(80))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(key, app, agent, handle, config, events, outcome_tx);
        }
    }
}

/// A short, human description of why a turn stopped other than by
/// answering normally; `None` for an ordinary end.
fn stop_notice(reason: &StopReason) -> Option<String> {
    Some(
        match reason {
            StopReason::EndTurn | StopReason::StopSequence | StopReason::ToolUse => return None,
            StopReason::MaxTokens => "the answer hit the output token limit",
            StopReason::Refusal => "the model declined to answer",
            StopReason::Cancelled => "turn cancelled",
            StopReason::Interrupted => "turn was interrupted",
            StopReason::ToolRoundLimit => "stopped: too many tool rounds in one turn",
            StopReason::TurnTokenLimit => "stopped: the turn's token budget ran out",
            StopReason::RepeatedToolCall => "stopped: the model kept repeating the same tool call",
            StopReason::Other(other) => return Some(format!("stopped: {other}")),
        }
        .to_string(),
    )
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
            Ok(RuntimeEvent::ThinkingDelta { .. }) => {
                app.activity = Some("thinking…".to_string());
            }
            Ok(RuntimeEvent::ModelStreamChunk { delta, .. }) => {
                app.activity = None;
                app.append_assistant_delta(&delta);
            }
            Ok(RuntimeEvent::ToolUseStarted { tool_name, .. }) => {
                app.activity = Some(format!("preparing {tool_name} call…"));
            }
            Ok(RuntimeEvent::ToolProgress { update, .. }) => {
                app.activity = Some(update);
            }
            Ok(RuntimeEvent::McpServerConnected { server, tools }) => {
                app.notice = Some(format!("MCP server {server} connected ({tools} tools)"));
            }
            Ok(RuntimeEvent::McpServerFailed { server, reason }) => {
                app.status_message = Some(format!("MCP server {server} unavailable: {reason}"));
            }
            Ok(RuntimeEvent::UsageUpdated { session, .. }) => {
                app.session_tokens = session.total_tokens();
            }
            Ok(RuntimeEvent::TurnCompleted { stop_reason, .. }) => {
                app.working = false;
                app.activity = None;
                if let Some(notice) = stop_notice(&stop_reason) {
                    app.notice = Some(notice);
                }
            }
            Ok(RuntimeEvent::TurnCancelled { .. }) => {
                app.working = false;
                app.activity = None;
                app.pending_approval = None;
                app.notice = Some("turn cancelled".to_string());
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
/// dump.
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
                // Cancellation is reported by the TurnCancelled event.
                if err != "cancelled" {
                    app.status_message = Some(err);
                }
            }
            AgentOutcome::ChatDone(Ok(_)) | AgentOutcome::ToolInvoked(Ok(())) => {}
            AgentOutcome::ToolInvoked(Err(err)) => {
                app.status_message = Some(format!("tool call failed: {err}"));
            }
        }
    }
}

/// Delivers a decision to the paused tool call. Model-initiated and
/// `/tool` calls wait on the same mailbox, so there's one path for both.
fn resolve_approval(app: &mut App, agent: &Agent, id: ToolCallId, decision: ApprovalDecision) {
    if !agent.supply_tool_decision(id, decision) {
        app.status_message = Some(format!(
            "tool call {id} is no longer waiting for a decision"
        ));
    }
}

/// Decrements the pending approval's countdown once per render-loop tick
/// (~80ms — see `event_loop`'s `event::poll` timeout) and auto-denies it
/// on expiry, per TUI spec §9 ("if approval prompt times out: default
/// action from policy, recommended deny... show user what action was
/// applied").
fn tick_approval_timeout(app: &mut App, agent: &Agent) {
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
    agent.supply_tool_decision(id, ApprovalDecision::DeniedOnce);
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    agent: &mut Arc<Agent>,
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
            KeyCode::Esc => {
                // Esc on the prompt cancels the whole turn, not just the call.
                app.pending_approval = None;
                agent.cancel_turn();
                return;
            }
            _ => None,
        };
        if let Some(decision) = decision {
            app.pending_approval = None;
            resolve_approval(app, agent, approval.id, decision);
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
                    resume_session(app, agent, config, events, session_id);
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
        (_, KeyCode::Esc) if app.working => {
            if agent.cancel_turn() {
                app.activity = Some("cancelling…".to_string());
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => {
            app.clear_transcript();
            app.scroll = 0;
            app.follow_tail = true;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('n')) => new_session(app, agent, config, events),
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

/// Replaces the running agent (new session / resume) and resets the view.
/// The old agent's turn, if any, is cancelled (it persists what it has) and
/// its session closed. The new agent shares the same `EventBus`, so the
/// existing event subscription keeps working.
fn swap_in_agent(app: &mut App, agent: &mut Arc<Agent>, new_agent: Agent) {
    agent.cancel_turn();
    let _ = agent.close();
    app.session_id = new_agent.session_id();
    app.profile = new_agent.profile().to_string();
    app.provider_name = new_agent.provider_name().to_string();
    app.model = new_agent.model().to_string();
    app.session_tokens = new_agent.usage().total_tokens();
    app.clear_transcript();
    app.pending_approval = None;
    app.proposed_tool_calls.clear();
    app.status_message = None;
    app.notice = None;
    app.working = false;
    app.activity = None;
    app.scroll = 0;
    app.follow_tail = true;
    *agent = Arc::new(new_agent);
}

fn new_session(
    app: &mut App,
    agent: &mut Arc<Agent>,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
) {
    match Agent::create(config, SessionStore::new(), events.clone()) {
        Ok(new_agent) => {
            swap_in_agent(app, agent, new_agent);
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
    agent: &mut Arc<Agent>,
    config: &RuntimeConfig,
    events: &Arc<EventBus>,
    session_id: SessionId,
) {
    let store = SessionStore::new();
    match Agent::resume(config, store.clone(), session_id, events.clone()) {
        Ok(new_agent) => {
            let turns = store.list_turns(session_id).unwrap_or_default();
            swap_in_agent(app, agent, new_agent);
            for turn in turns {
                if let Some(m) = turn.user_message() {
                    app.push_line(Role::User, m.text());
                }
                let tool_calls: usize = turn.messages.iter().map(|m| m.tool_uses().len()).sum();
                if tool_calls > 0 {
                    app.push_line(Role::Tool, format!("({tool_calls} tool call(s))"));
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
    agent: &Arc<Agent>,
    handle: &Handle,
    outcome_tx: &Sender<AgentOutcome>,
) {
    if app.input.trim().is_empty() {
        return;
    }
    if agent.is_busy() {
        // Keep what was typed; nothing is sent until the turn ends.
        app.notice =
            Some("a turn is in progress — wait for it, or press Esc to cancel".to_string());
        return;
    }
    let content = app.take_input();
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
        let tx = outcome_tx.clone();
        // Same gated path as a model-initiated call: its approval prompt
        // and result arrive as runtime events.
        handle.spawn(async move {
            let result = agent
                .invoke_tool(name, arguments)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string());
            let _ = tx.send(AgentOutcome::ToolInvoked(result));
        });
        return;
    }

    app.push_line(Role::User, content.clone());
    app.working = true;
    app.activity = Some("sending…".to_string());
    let agent = agent.clone();
    let tx = outcome_tx.clone();
    handle.spawn(async move {
        let result = agent
            .submit_message(content)
            .await
            .map_err(|e| e.to_string());
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
    fn stop_notices_explain_guarded_stops_but_not_normal_ends() {
        assert_eq!(stop_notice(&StopReason::EndTurn), None);
        assert!(
            stop_notice(&StopReason::ToolRoundLimit)
                .unwrap()
                .contains("tool rounds")
        );
        assert_eq!(
            stop_notice(&StopReason::Other("x".into())).as_deref(),
            Some("stopped: x")
        );
    }

    #[test]
    fn a_cancelled_turn_clears_working_state_and_any_open_prompt() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut app = test_app();
        app.working = true;
        app.pending_approval = Some(PendingApproval {
            id: ToolCallId::new(),
            tool_name: "execute".into(),
            arguments_pretty: "{}".into(),
            risk: arbe_runtime::arbe_core::RiskLevel::High,
            source_turn: TurnId::new(),
            ticks_remaining: 10,
        });
        tx.send(EventEnvelope {
            seq: 0,
            event: RuntimeEvent::TurnCancelled {
                session_id: app.session_id,
                turn_id: TurnId::new(),
            },
        })
        .unwrap();

        drain_runtime_events(&mut app, &mut rx);

        assert!(!app.working);
        assert!(app.pending_approval.is_none());
        assert_eq!(app.notice.as_deref(), Some("turn cancelled"));
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
                stop_reason: StopReason::EndTurn,
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
                    stop_reason: StopReason::EndTurn,
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
