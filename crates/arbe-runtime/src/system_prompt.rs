//! Placeholder-based system prompt composition (docs/tmp/system-prompt.md).
//! A template is Markdown text with optional `{global_instructions}` and
//! `{project_instructions}` placeholders; the built-in wordings are
//! stand-ins until real templates are written, but the substitution
//! contract is what other code relies on.

use std::path::PathBuf;

/// Which system prompt template a profile uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptTemplate {
    /// Software-engineering agent working on the project directory.
    Coding,
    /// General-purpose assistant; no assumptions about code or files.
    General,
    /// A Markdown file with the user's own template, re-read every turn.
    File(PathBuf),
}

impl PromptTemplate {
    /// `"coding"`, `"general"`, or anything else as a file path.
    pub fn parse(value: &str, relative_to: Option<&std::path::Path>) -> Self {
        match value {
            "coding" => Self::Coding,
            "general" => Self::General,
            path => {
                let path = PathBuf::from(path);
                match relative_to {
                    Some(base) if path.is_relative() => Self::File(base.join(path)),
                    _ => Self::File(path),
                }
            }
        }
    }

    /// The template text. A custom file that can't be read falls back to
    /// the coding template (with a warning), so a moved file degrades the
    /// prompt instead of breaking every turn.
    pub fn text(&self) -> String {
        match self {
            Self::Coding => CODING_TEMPLATE.to_string(),
            Self::General => GENERAL_TEMPLATE.to_string(),
            Self::File(path) => std::fs::read_to_string(path).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), %err, "prompt template unreadable; using the coding template");
                CODING_TEMPLATE.to_string()
            }),
        }
    }
}

const GENERAL_TEMPLATE: &str = "You are ArBeHarness, a helpful general-purpose assistant. Answer questions, help with writing and analysis, and work through problems step by step.

Use the tools you have when they help; don't assume you can read or change files unless a tool for it is available.

Keep responses clear and direct: lead with the answer, then the detail that supports it. Say so when you're unsure rather than guessing.

{global_instructions}

{project_instructions}";

const CODING_TEMPLATE: &str = "\
You are ArBeHarness, an autonomous agent that completes software engineering tasks: solving bugs, adding functionality, refactoring, and explaining code. Your main goal is to complete the user's request.

You are highly capable and often allow users to complete ambitious tasks that would otherwise be too complex or take too long. Defer to the user's judgement about whether a task is too large to attempt.

If you intend to call multiple tools and there are no dependencies between them, make all independent calls together rather than one at a time.

Don't add features, refactor, or add error handling/validation beyond what the task requires. Three similar lines of code is better than a premature abstraction. Trust internal code and framework guarantees; only validate at real boundaries (user input, external APIs).

Before reporting a task complete, verify it actually works — run the test, execute the script, check the output. If you can't verify, say so explicitly rather than claiming success.

Keep responses brief and direct: lead with the action or answer, skip restating what the user said. When referencing code, use the pattern file_path:line_number.

{global_instructions}

{project_instructions}";

/// Caps how much of a single instructions file gets folded into the system
/// prompt. `ContextPipeline` charges system instructions as a fixed cost
/// against the token budget before history sees any of it (see
/// `arbe-memory`'s `pipeline.rs`), so an unbounded `agent.md`/`CLAUDE.md`
/// could otherwise crowd out all history for a turn.
const MAX_SECTION_CHARS: usize = 8_000;

/// Renders the coding template — see [`render_template`].
pub fn render_system_prompt(global: Option<&str>, project: Option<&str>) -> String {
    render_template(CODING_TEMPLATE, global, project)
}

/// Renders `template` by substituting `{global_instructions}` and
/// `{project_instructions}` with the given content, each capped at
/// `MAX_SECTION_CHARS`. A missing section (`None`) substitutes an empty
/// string rather than omitting the placeholder text itself, so the
/// template's surrounding wording never needs to special-case absence.
///
/// Each placeholder is optional and replaced at most once (a custom
/// template may leave either out). Substitution is a single pass over the
/// template's own text, never over already-inserted content: if `global`
/// contained the literal `"{project_instructions}"` (plausible in an
/// `agent.md` that documents this very contract), a second `.replace()`
/// pass would splice the project section into it.
pub fn render_template(template: &str, global: Option<&str>, project: Option<&str>) -> String {
    let mut slots: Vec<(usize, &str, String)> = [
        ("{global_instructions}", global),
        ("{project_instructions}", project),
    ]
    .into_iter()
    .filter_map(|(marker, value)| template.find(marker).map(|at| (at, marker, cap(value))))
    .collect();
    slots.sort_by_key(|(at, _, _)| *at);

    let mut rendered = String::with_capacity(template.len());
    let mut cursor = 0;
    for (at, marker, value) in slots {
        rendered.push_str(&template[cursor..at]);
        rendered.push_str(&value);
        cursor = at + marker.len();
    }
    rendered.push_str(&template[cursor..]);
    rendered
}

fn cap(section: Option<&str>) -> String {
    match section {
        None => String::new(),
        Some(text) if text.len() <= MAX_SECTION_CHARS => text.to_string(),
        Some(text) => {
            // Truncate on a char boundary, not a raw byte offset, so a
            // multi-byte UTF-8 codepoint straddling the cap doesn't panic.
            let mut end = MAX_SECTION_CHARS;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            format!(
                "{}\n...[truncated, {} bytes omitted]",
                &text[..end],
                text.len() - end
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_both_sections_when_present() {
        let rendered = render_system_prompt(Some("be terse"), Some("this is a Rust repo"));
        assert!(rendered.contains("be terse"));
        assert!(rendered.contains("this is a Rust repo"));
    }

    #[test]
    fn missing_sections_become_empty_not_literal_placeholder_text() {
        let rendered = render_system_prompt(None, None);
        assert!(!rendered.contains("{global_instructions}"));
        assert!(!rendered.contains("{project_instructions}"));
    }

    #[test]
    fn global_instructions_containing_the_other_placeholder_literal_is_not_spliced_into() {
        // If global instructions happen to contain the literal text
        // "{project_instructions}" (plausible in a hand-written
        // agent.md/CLAUDE.md that documents this very substitution
        // contract), a naive two-pass `.replace()` would rescan it on the
        // second pass and splice the project section into the middle of
        // the global section instead of leaving it untouched.
        let global = "be terse. uses the {project_instructions} placeholder internally.";
        let rendered = render_system_prompt(Some(global), Some("this is a Rust repo"));

        assert!(
            rendered.contains(global),
            "global text must survive verbatim"
        );
        // The project section must appear exactly once, in its own slot —
        // not injected a second time into the middle of the global text.
        assert_eq!(rendered.matches("this is a Rust repo").count(), 1);
    }

    #[test]
    fn custom_templates_may_omit_or_reorder_placeholders() {
        assert_eq!(
            render_template(
                "P: {project_instructions} G: {global_instructions}",
                Some("g"),
                Some("p")
            ),
            "P: p G: g"
        );
        assert_eq!(
            render_template("no slots here", Some("g"), Some("p")),
            "no slots here"
        );
    }

    #[test]
    fn templates_are_chosen_by_name_or_path() {
        assert_eq!(
            PromptTemplate::parse("coding", None),
            PromptTemplate::Coding
        );
        assert_eq!(
            PromptTemplate::parse("general", None),
            PromptTemplate::General
        );
        assert_eq!(
            PromptTemplate::parse("prompts/mine.md", Some(std::path::Path::new("/cfg"))),
            PromptTemplate::File(std::path::Path::new("/cfg").join("prompts/mine.md"))
        );
        assert!(PromptTemplate::General.text().contains("general-purpose"));
        // An unreadable custom template degrades to the coding one.
        assert_eq!(
            PromptTemplate::File("/definitely/not/here.md".into()).text(),
            PromptTemplate::Coding.text()
        );
    }

    #[test]
    fn oversized_section_is_truncated_with_a_marker() {
        let huge = "x".repeat(MAX_SECTION_CHARS + 500);
        let rendered = render_system_prompt(Some(&huge), None);
        assert!(rendered.contains("truncated"));
        assert!(
            !rendered.contains(&huge),
            "the full oversized section should not survive into the rendered prompt"
        );
    }
}
