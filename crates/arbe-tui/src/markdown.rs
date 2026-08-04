//! Minimal, dependency-free markdown-to-`Line` renderer for the transcript
//! pane. TUI spec §7 only requires markdown be "preserved as plain text...
//! light formatting optional" — this covers the common cases a model's
//! output actually uses (headings, bold/italic/code, fenced code blocks,
//! lists, blockquotes, rules) without pulling in a full CommonMark parser.
//! Line count in == line count out (one raw line -> one rendered `Line`,
//! never merged/split), so `App::content_line_count`'s scroll math stays
//! correct without needing to know about markdown at all.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

pub fn render_markdown(content: &str, base_style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    for raw_line in content.split('\n') {
        let trimmed = raw_line.trim_start();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            lines.push(Line::from(Span::styled(
                raw_line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        if in_code_block {
            lines.push(Line::from(Span::styled(
                raw_line.to_string(),
                Style::default().fg(Color::Green),
            )));
            continue;
        }
        if let Some(text) = heading_text(trimmed) {
            lines.push(Line::from(Span::styled(
                text.to_string(),
                base_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
            continue;
        }
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "\u{2500}".repeat(40),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled(
                "\u{2502} ",
                Style::default().fg(Color::DarkGray),
            )];
            spans.extend(parse_inline(
                rest,
                base_style.add_modifier(Modifier::ITALIC),
            ));
            lines.push(Line::from(spans));
            continue;
        }
        if let Some(rest) = bullet_text(trimmed) {
            let mut spans = vec![Span::styled("\u{2022} ", base_style)];
            spans.extend(parse_inline(rest, base_style));
            lines.push(Line::from(spans));
            continue;
        }
        lines.push(Line::from(parse_inline(raw_line, base_style)));
    }
    lines
}

fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
        Some(line[hashes + 1..].trim())
    } else {
        None
    }
}

fn bullet_text(line: &str) -> Option<&str> {
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    line[digits.len()..].strip_prefix(". ")
}

/// Parses inline `` `code` ``, `**bold**`, and `*italic*`/`_italic_` spans.
/// Deliberately not recursive/nested — good enough for chat output, not a
/// full CommonMark inline grammar.
fn parse_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '`' {
            if let Some(end) = find_marker(&chars, i + 1, '`') {
                flush(&mut buf, &mut spans, base);
                let code: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    code,
                    base.fg(Color::Magenta).add_modifier(Modifier::BOLD),
                ));
                i = end + 1;
                continue;
            }
        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = find_double_marker(&chars, i + 2, '*') {
                flush(&mut buf, &mut spans, base);
                let bold: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(bold, base.add_modifier(Modifier::BOLD)));
                i = end + 2;
                continue;
            }
        } else if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            if let Some(end) = find_marker(&chars, i + 1, marker)
                && end > i + 1
            {
                flush(&mut buf, &mut spans, base);
                let italic: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(italic, base.add_modifier(Modifier::ITALIC)));
                i = end + 1;
                continue;
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut spans, base);
    spans
}

fn flush(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn find_marker(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == marker)
}

fn find_double_marker(chars: &[char], from: usize, marker: char) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&j| chars[j] == marker && chars[j + 1] == marker)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn preserves_one_rendered_line_per_raw_line() {
        let content = "line one\nline two\nline three";
        let rendered = render_markdown(content, Style::default());
        assert_eq!(rendered.len(), 3);
    }

    #[test]
    fn strips_heading_markers() {
        let rendered = render_markdown("## Section Title", Style::default());
        assert_eq!(plain_text(&rendered[0]), "Section Title");
    }

    #[test]
    fn renders_bold_italic_and_code_as_separate_spans() {
        let rendered = render_markdown("a **bold** and `code` and *em*", Style::default());
        let texts: Vec<String> = rendered[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(texts.contains(&"bold".to_string()));
        assert!(texts.contains(&"code".to_string()));
        assert!(texts.contains(&"em".to_string()));
    }

    #[test]
    fn code_fences_toggle_a_no_inline_parsing_zone() {
        let content = "before\n```\n**not bold**\n```\nafter";
        let rendered = render_markdown(content, Style::default());
        assert_eq!(rendered.len(), 5);
        assert_eq!(plain_text(&rendered[2]), "**not bold**");
    }

    #[test]
    fn bullet_lines_get_a_bullet_glyph() {
        let rendered = render_markdown("- item one", Style::default());
        assert!(plain_text(&rendered[0]).starts_with('\u{2022}'));
    }
}
