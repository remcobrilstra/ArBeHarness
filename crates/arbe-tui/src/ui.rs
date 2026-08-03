use arbe_runtime::arbe_core::Role;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::app::App;

/// Renders the 3-region layout from TUI spec §4: header/status bar, main
/// transcript pane, input bar + hints — plus the tool-approval modal when
/// one is pending.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    draw_header(frame, chunks[0], app);
    draw_transcript(frame, chunks[1], app);
    draw_input(frame, chunks[2], app);

    if let Some(approval) = &app.pending_approval {
        draw_approval_modal(frame, area, approval);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let phase = if app.working { "working" } else { "idle" };
    let text = format!(
        " profile: {}  |  provider: {}  |  model: {}  |  session: {}  |  phase: {}  |  ~tokens: {} ",
        app.profile, app.provider_name, app.model, app.session_id, phase, app.last_estimated_tokens
    );
    let paragraph = Paragraph::new(text)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("ArBeHarness"));
    frame.render_widget(paragraph, area);
}

fn role_style(role: Role) -> Style {
    match role {
        Role::User => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Role::Assistant => Style::default().fg(Color::White),
        Role::System | Role::Tool => Style::default().fg(Color::DarkGray),
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::User => "you",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.transcript {
        lines.push(Line::from(vec![Span::styled(
            format!("[{}] ", role_label(entry.role)),
            role_style(entry.role),
        )]));
        for wrapped in entry.content.split('\n') {
            lines.push(Line::from(Span::styled(
                wrapped.to_string(),
                role_style(entry.role),
            )));
        }
    }
    if let Some(status) = &app.status_message {
        lines.push(Line::from(Span::styled(
            format!("[error] {status}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }

    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("transcript"))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let hint = if app.pending_approval.is_some() {
        "approval pending — see modal"
    } else {
        "Enter: send  |  Ctrl+L: clear  |  Ctrl+C: quit  |  /tool <name> <json>: propose a demo tool call"
    };
    let text = format!("> {}", app.input);
    let paragraph = Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(hint));
    frame.render_widget(paragraph, area);
}

fn draw_approval_modal(frame: &mut Frame, area: Rect, approval: &crate::app::PendingApproval) {
    let width = area.width.saturating_sub(10).clamp(30, 70);
    let height = 9u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);

    let text = vec![
        Line::from(Span::styled(
            format!("Tool call: {}", approval.tool_name),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("arguments: {}", approval.arguments_pretty)),
        Line::from(""),
        Line::from("[y] approve once   [n] deny once"),
        Line::from("[a] approve for session   [d] always deny for session"),
    ];
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Tool Approval Required")
                .style(Style::default().fg(Color::Yellow)),
        );
    frame.render_widget(paragraph, popup);
}
