use arbe_runtime::arbe_core::{RiskLevel, Role};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;

/// Renders the 3-region layout from TUI spec §4: header/status bar, main
/// transcript pane, input bar + hints — plus modal overlays (tool
/// approval, session picker) when active.
///
/// Takes `&mut App` (not `&App`) because the transcript viewport height is
/// only known here, at render time, and is needed by key handling on the
/// *next* iteration to clamp/auto-follow scroll (see `App::max_scroll`).
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(4),
        ])
        .split(area);

    if app.working {
        app.tick_spinner();
    }
    draw_header(frame, chunks[0], app);
    draw_transcript(frame, chunks[1], app);
    draw_input(frame, chunks[2], app);

    if let Some(approval) = &app.pending_approval {
        draw_approval_modal(frame, area, approval);
    } else if let Some(picker) = &app.session_picker {
        draw_session_picker(frame, area, picker);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let phase = if app.working {
        format!(
            "{} {}",
            app.spinner_glyph(),
            app.activity.as_deref().unwrap_or("working")
        )
    } else {
        "idle".to_string()
    };
    let lines = vec![
        Line::from(format!(" workdir: {} ", app.project_dir)),
        Line::from(format!(
            " profile: {}  |  provider: {}  |  model: {}  |  session: {}  |  phase: {}  |  ~tokens: {} ",
            app.profile,
            app.provider_name,
            app.model,
            app.session_id,
            phase,
            app.last_estimated_tokens
        )),
    ];
    let style = if app.working {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let paragraph = Paragraph::new(lines)
        .style(style)
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

/// Renders one transcript entry as light markdown (`crate::markdown`) with
/// the role label inlined onto the first line (rather than its own line, so
/// a short reply doesn't cost two rows of vertical space). One raw content
/// line always maps to exactly one rendered `Line`, which is what keeps
/// `App::content_line_count`'s scroll math correct without it needing to
/// know about markdown.
fn render_entry(entry: &crate::app::TranscriptLine) -> Vec<Line<'static>> {
    let base_style = role_style(entry.role);
    let mut rendered = crate::markdown::render_markdown(&entry.content, base_style);
    let mut first = if rendered.is_empty() {
        Line::default()
    } else {
        rendered.remove(0)
    };
    let mut first_spans = vec![Span::styled(
        format!("[{}] ", role_label(entry.role)),
        base_style.add_modifier(Modifier::BOLD),
    )];
    first_spans.append(&mut first.spans);
    let mut lines = vec![Line::from(first_spans)];
    lines.extend(rendered);
    lines
}

/// Builds the transcript's display lines. Every entry except the last is
/// immutable once appended (only the last one ever grows in place, via
/// streaming deltas — see `App::append_assistant_delta`), so `app`'s
/// `rendered_cache` holds their rendering across frames and this only ever
/// does fresh markdown-parsing work for entries that just became settled
/// plus the current last entry — not the whole transcript, every ~80ms
/// render tick, for the life of the session.
fn transcript_lines(app: &mut App) -> Vec<Line<'static>> {
    let settled_count = app.transcript.len().saturating_sub(1);
    if app.rendered_cache_entry_count < settled_count {
        for entry in &app.transcript[app.rendered_cache_entry_count..settled_count] {
            app.rendered_cache.extend(render_entry(entry));
        }
        app.rendered_cache_entry_count = settled_count;
    }

    let mut lines = app.rendered_cache.clone();
    if let Some(last) = app.transcript.last() {
        lines.extend(render_entry(last));
    }
    if let Some(status) = &app.status_message {
        lines.push(Line::from(Span::styled(
            format!("[error] {status}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(notice) = &app.notice {
        lines.push(Line::from(Span::styled(
            format!("[info] {notice}"),
            Style::default().fg(Color::Cyan),
        )));
    }
    lines
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let viewport_height = area.height.saturating_sub(2); // top/bottom borders
    app.last_viewport_height = viewport_height;
    if app.follow_tail {
        app.scroll = app.max_scroll();
    } else {
        app.scroll = app.scroll.min(app.max_scroll());
    }

    let lines = transcript_lines(app);
    let scroll_hint = if app.max_scroll() > 0 && !app.follow_tail {
        format!(
            "transcript (PgUp/PgDn to scroll, {}/{})",
            app.scroll,
            app.max_scroll()
        )
    } else {
        "transcript".to_string()
    };
    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(scroll_hint))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll, 0));
    frame.render_widget(paragraph, area);
}

/// Row/column (0-indexed) of `app.input_cursor` within the input buffer,
/// used to position the terminal cursor. Column is a char count, not a
/// display-width-aware wrap column — good enough for v1 (TUI spec §7 only
/// requires "wrap correctly and remain scrollable" for transcript output,
/// not input editing).
fn cursor_row_col(app: &App) -> (u16, u16) {
    let before = &app.input[..app.input_cursor];
    let row = before.matches('\n').count() as u16;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() as u16;
    (row, col)
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    let hint = if app.pending_approval.is_some() {
        "approval pending — see modal"
    } else if app.session_picker.is_some() {
        "session picker — see modal"
    } else {
        "Enter: send | Shift/Alt+Enter: newline | \u{2191}/\u{2193}/PgUp/PgDn: scroll | Ctrl+N: new | Ctrl+R: resume | Ctrl+C: quit"
    };
    let text = app.input.as_str();
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(hint));
    frame.render_widget(paragraph, area);

    let (row, col) = cursor_row_col(app);
    let cursor_x = area.x + 1 + col;
    let cursor_y = area.y + 1 + row;
    if cursor_x < area.x + area.width.saturating_sub(1)
        && cursor_y < area.y + area.height.saturating_sub(1)
    {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn risk_style(risk: RiskLevel) -> Style {
    match risk {
        RiskLevel::Low => Style::default().fg(Color::Green),
        RiskLevel::Medium => Style::default().fg(Color::Yellow),
        RiskLevel::High => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn risk_label(risk: RiskLevel) -> &'static str {
    match risk {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

fn draw_approval_modal(frame: &mut Frame, area: Rect, approval: &crate::app::PendingApproval) {
    let width = area.width.saturating_sub(10).clamp(30, 70);
    let height = 13u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);

    let seconds_left = approval.ticks_remaining * 80 / 1000;
    let text = vec![
        Line::from(Span::styled(
            format!("Tool call: {}", approval.tool_name),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw("risk: "),
            Span::styled(risk_label(approval.risk), risk_style(approval.risk)),
            Span::raw(format!("   source turn: {}", approval.source_turn)),
        ]),
        Line::from(""),
        Line::from(format!("arguments: {}", approval.arguments_pretty)),
        Line::from(""),
        Line::from("[y] approve once   [n] deny once"),
        Line::from("[a] approve for session   [d] always deny for session"),
        // "Approve for session" doesn't cover high-risk tools unless config
        // opts in (arbe_tools::ApprovalContext::session_approval_covers_high_risk),
        // so say so rather than let [a] look broken when it asks again.
        if approval.risk == RiskLevel::High {
            Line::from(Span::styled(
                "(high risk: [a] still asks again next call unless config allows it)",
                Style::default().fg(Color::DarkGray),
            ))
        } else {
            Line::from("")
        },
        Line::from(Span::styled(
            format!("auto-deny in {seconds_left}s if no response"),
            Style::default().fg(Color::DarkGray),
        )),
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

fn draw_session_picker(frame: &mut Frame, area: Rect, picker: &crate::app::SessionPicker) {
    let width = area.width.saturating_sub(10).clamp(40, 90);
    let height = (picker.sessions.len() as u16 + 4)
        .min(area.height.saturating_sub(4))
        .max(6);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = if picker.sessions.is_empty() {
        vec![ListItem::new("no previous sessions found")]
    } else {
        picker
            .sessions
            .iter()
            .map(|meta| {
                ListItem::new(format!(
                    "{}  {}  {}/{}  updated {}",
                    meta.id,
                    format!("{:?}", meta.status).to_lowercase(),
                    meta.provider,
                    meta.model,
                    meta.updated_at.format("%Y-%m-%d %H:%M:%S"),
                ))
            })
            .collect()
    };

    let mut state = ListState::default();
    if !picker.sessions.is_empty() {
        state.select(Some(picker.selected));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Resume Session — Up/Down, Enter to resume, Esc to cancel"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, popup, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_runtime::arbe_core::SessionId;

    fn test_app() -> App {
        App::new(
            SessionId::new(),
            "default".to_string(),
            "fake".to_string(),
            "fake-model".to_string(),
            ".".to_string(),
        )
    }

    fn plain_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn transcript_lines_matches_uncached_rendering() {
        let mut app = test_app();
        app.push_line(Role::User, "hello".to_string());
        app.push_line(Role::Assistant, "hi there".to_string());
        app.push_line(Role::User, "another one".to_string());

        let rendered = transcript_lines(&mut app);
        let text = plain_text(&rendered);
        assert!(text.contains("hello"));
        assert!(text.contains("hi there"));
        assert!(text.contains("another one"));
    }

    #[test]
    fn only_the_last_entry_is_excluded_from_the_settled_cache() {
        let mut app = test_app();
        app.push_line(Role::User, "one".to_string());
        app.push_line(Role::Assistant, "two".to_string());

        transcript_lines(&mut app);
        // Two entries total; the last (index 1) is never cached since it
        // may still be streamed into.
        assert_eq!(app.rendered_cache_entry_count, 1);
    }

    #[test]
    fn streaming_deltas_into_the_last_entry_do_not_grow_the_settled_cache() {
        let mut app = test_app();
        app.push_line(Role::User, "one".to_string());
        app.append_assistant_delta("partial");
        app.working = true;

        transcript_lines(&mut app);
        let cache_len_before = app.rendered_cache.len();

        app.append_assistant_delta(" more");
        let rendered = transcript_lines(&mut app);

        // The settled cache (everything but the streaming last entry)
        // shouldn't have grown just because the last entry got longer.
        assert_eq!(app.rendered_cache.len(), cache_len_before);
        assert!(plain_text(&rendered).contains("partial more"));
    }

    #[test]
    fn clear_transcript_resets_the_cache_too() {
        let mut app = test_app();
        app.push_line(Role::User, "one".to_string());
        app.push_line(Role::Assistant, "two".to_string());
        transcript_lines(&mut app);
        assert_ne!(app.rendered_cache_entry_count, 0);

        app.clear_transcript();

        assert_eq!(app.rendered_cache_entry_count, 0);
        assert!(app.rendered_cache.is_empty());
        assert!(app.transcript.is_empty());
    }
}
