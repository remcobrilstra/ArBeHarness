//! Terminal UI (TUI spec). Depends only on `arbe-runtime`'s event/command
//! contract (reached via its re-exports of `arbe_core`/`arbe_tools`), and
//! contains no agent decision logic (TUI spec §2) — everything here is
//! either presentation state (`app.rs`) or rendering (`ui.rs`).

pub mod app;
pub mod ui;

pub use arbe_runtime;

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use arbe_runtime::Agent;
use arbe_runtime::arbe_core::{ApprovalDecision, Role, RuntimeEvent, ToolCallId};
use arbe_runtime::arbe_tools::ToolExecutor;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::runtime::Handle;
use tokio::sync::Mutex as AsyncMutex;

use app::{App, PendingApproval};

/// Result of a spawned agent call, delivered back to the render loop so it
/// never has to block on the async work itself.
enum AgentOutcome {
    ChatDone(Result<String, String>),
    ToolResolved(ToolCallId, Result<String, String>),
}

/// Runs the TUI until the user quits. Blocking: call this from a dedicated
/// OS thread (e.g. `tokio::task::spawn_blocking`) — it drives its own
/// render/input loop and uses `handle` to run the agent's async methods
/// from that thread via `Handle::block_on`/`Handle::spawn`.
pub fn run(agent: Agent, handle: Handle) -> io::Result<()> {
    let mut events_rx = agent.subscribe_events();
    let profile = agent.profile().to_string();
    let provider_name = agent.provider_name().to_string();
    let model = agent.model().to_string();
    let session_id = agent.session_id();

    let agent = Arc::new(AsyncMutex::new(agent));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(session_id, profile, provider_name, model);
    let (outcome_tx, outcome_rx) = channel::<AgentOutcome>();

    let result = event_loop(
        &mut terminal,
        &mut app,
        &agent,
        &handle,
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
    handle: &Handle,
    events_rx: &mut tokio::sync::broadcast::Receiver<RuntimeEvent>,
    outcome_tx: &Sender<AgentOutcome>,
    outcome_rx: &Receiver<AgentOutcome>,
) -> io::Result<()> {
    loop {
        drain_runtime_events(app, events_rx);
        drain_outcomes(app, outcome_rx);

        terminal.draw(|f| ui::draw(f, app))?;

        if app.should_quit {
            return Ok(());
        }

        if event::poll(Duration::from_millis(80))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(key, app, agent, handle, outcome_tx);
        }
    }
}

fn drain_runtime_events(
    app: &mut App,
    events_rx: &mut tokio::sync::broadcast::Receiver<RuntimeEvent>,
) {
    loop {
        match events_rx.try_recv() {
            Ok(RuntimeEvent::ModelStreamChunk { delta, .. }) => app.append_assistant_delta(&delta),
            Ok(RuntimeEvent::TurnCompleted { .. }) => app.working = false,
            Ok(RuntimeEvent::RuntimeError { reason, .. }) => {
                app.working = false;
                app.status_message = Some(reason);
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }
}

fn drain_outcomes(app: &mut App, outcome_rx: &Receiver<AgentOutcome>) {
    while let Ok(outcome) = outcome_rx.try_recv() {
        app.working = false;
        match outcome {
            AgentOutcome::ChatDone(Err(err)) => app.status_message = Some(err),
            AgentOutcome::ChatDone(Ok(_)) => {}
            AgentOutcome::ToolResolved(id, Err(err)) => {
                app.status_message = Some(format!("tool call {id} failed: {err}"))
            }
            AgentOutcome::ToolResolved(id, Ok(summary)) => {
                app.push_line(Role::System, format!("tool call {id} result: {summary}"))
            }
        }
    }
}

fn handle_key(
    key: crossterm::event::KeyEvent,
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    handle: &Handle,
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
            let agent = agent.clone();
            let tx = outcome_tx.clone();
            let id = approval.id;
            handle.spawn(async move {
                let mut a = agent.lock().await;
                let result = a
                    .resolve_tool_call(id, decision)
                    .await
                    .map(|outcome| format!("{outcome:?}"))
                    .map_err(|e| e.to_string());
                let _ = tx.send(AgentOutcome::ToolResolved(id, result));
            });
        }
        return;
    }

    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => app.should_quit = true,
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => app.transcript.clear(),
        (_, KeyCode::Enter) => submit_input(app, agent, handle, outcome_tx),
        (_, KeyCode::Backspace) => {
            app.input.pop();
        }
        (_, KeyCode::Char(c)) => app.input.push(c),
        _ => {}
    }
}

fn submit_input(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    handle: &Handle,
    outcome_tx: &Sender<AgentOutcome>,
) {
    let content = std::mem::take(&mut app.input);
    if content.is_empty() {
        return;
    }
    app.status_message = None;

    if let Some(rest) = content.strip_prefix("/tool ") {
        let Some((name, args_text)) = rest.split_once(' ') else {
            app.status_message = Some("usage: /tool <name> <json-args>".to_string());
            return;
        };
        let name = name.to_string();
        let arguments: serde_json::Value =
            serde_json::from_str(args_text).unwrap_or(serde_json::json!({}));
        let pretty = serde_json::to_string(&arguments).unwrap_or_default();

        let agent = agent.clone();
        let name_for_task = name.clone();
        let arguments_for_task = arguments.clone();
        let id = handle.block_on(async move {
            let mut a = agent.lock().await;
            a.propose_tool_call(
                name_for_task,
                arguments_for_task,
                arbe_runtime::arbe_core::RiskLevel::Medium,
            )
        });
        app.pending_approval = Some(PendingApproval {
            id,
            tool_name: name,
            arguments_pretty: pretty,
        });
        return;
    }

    app.push_line(Role::User, content.clone());
    app.working = true;
    let agent = agent.clone();
    let tx = outcome_tx.clone();
    handle.spawn(async move {
        let mut a = agent.lock().await;
        let result = a.submit_message(content).await.map_err(|e| e.to_string());
        let _ = tx.send(AgentOutcome::ChatDone(result));
    });
}
