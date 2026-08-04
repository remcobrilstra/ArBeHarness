/// Placeholder-based system prompt composition (docs/tmp/system-prompt.md).
/// Deliberately temporary: the exact wording below is a stand-in until a
/// real template is written, but the substitution mechanism
/// (`{global_instructions}`/`{project_instructions}`) is the actual
/// contract other code should rely on.
const TEMPLATE: &str = "\
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

/// Renders [`TEMPLATE`] by substituting `{global_instructions}` and
/// `{project_instructions}` with the given content, each capped at
/// `MAX_SECTION_CHARS`. A missing section (`None`) substitutes an empty
/// string rather than omitting the placeholder text itself, so the
/// template's surrounding wording never needs to special-case absence.
///
/// Splits `TEMPLATE` around both placeholders up front and assembles the
/// result from the pieces, rather than doing two sequential `.replace()`
/// passes over the same growing string — a sequential replace would rescan
/// text already substituted in by the first pass, so if `global`'s content
/// happened to contain the literal substring `"{project_instructions}"`
/// (plausible for a hand-written `agent.md`/`CLAUDE.md`), the second
/// `.replace` would splice the project instructions into the middle of the
/// already-inserted global instructions instead of leaving them alone.
pub fn render_system_prompt(global: Option<&str>, project: Option<&str>) -> String {
    let (before, rest) = TEMPLATE
        .split_once("{global_instructions}")
        .expect("TEMPLATE must contain the {global_instructions} placeholder");
    let (middle, after) = rest
        .split_once("{project_instructions}")
        .expect("TEMPLATE must contain the {project_instructions} placeholder");

    let mut rendered = String::with_capacity(TEMPLATE.len());
    rendered.push_str(before);
    rendered.push_str(&cap(global));
    rendered.push_str(middle);
    rendered.push_str(&cap(project));
    rendered.push_str(after);
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
