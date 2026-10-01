use std::path::PathBuf;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use arbe_storage::atomic::write_atomic;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::path_guard::{resolve_within_root, verify_no_symlink_escape};
use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Mirrors `read_file::MAX_READ_BYTES` — an edit reads the whole file into
/// memory before applying the find/replace, so it needs the same guard
/// against a single tool call pulling an entire large file into memory.
const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
struct Args {
    /// Path relative to the project root.
    path: String,
    /// Exact text to find. Line endings don't need to match: in a file that
    /// uses LF or CRLF throughout, `\n` and `\r\n` are converted to the file's.
    find: String,
    /// Text to put in its place (line endings converted the same way). Must
    /// differ from `find`.
    replace: String,
    /// Replace every occurrence. Defaults to false, which fails on more than one match.
    #[serde(default)]
    replace_all: bool,
}

/// Targeted find/replace within a file — safer than `write_file` for small
/// changes, since it fails loudly if `find` doesn't match rather than
/// silently doing nothing, and (by default) fails if it matches more than
/// once, so an ambiguous edit doesn't quietly land in the wrong spot.
pub struct EditFileTool {
    root: PathBuf,
}

impl EditFileTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for EditFileTool {
    fn subject_kind(&self) -> crate::SubjectKind {
        crate::SubjectKind::Path
    }

    /// Rule subject: the path (see `ToolExecutor::subject`).
    fn subject(&self, arguments: &serde_json::Value) -> Option<String> {
        crate::path_subject(arguments, None)
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Find-and-replace a substring within an existing file. Fails if `find` doesn't match, or matches more than once unless replace_all is set. If `find` has no exact match, whole lines are compared ignoring trailing whitespace, then indentation (the replacement is re-indented); the result's `matched` says when that happened.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    /// Not parallel-safe: it rewrites a file.
    fn parallel_safe(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid edit_file arguments: {e}")))?;
        let path = resolve_within_root(&self.root, &args.path)?;
        verify_no_symlink_escape(&self.root, &path).await?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;
        if metadata.len() > MAX_READ_BYTES {
            return Err(ToolError::Validation(format!(
                "{} is {} bytes, over the {MAX_READ_BYTES}-byte read limit",
                path.display(),
                metadata.len()
            )));
        }

        let original = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;

        let edited = apply_edit(&original, &args.find, &args.replace, args.replace_all)?;

        let write_path = path.clone();
        let text = edited.text;
        tokio::task::spawn_blocking(move || write_atomic(&write_path, text.as_bytes()))
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("write task panicked: {e}")))?
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;

        Ok(ToolResult {
            id: invocation.id,
            output: match edited.loose {
                Some(how) => json!({ "replacements": edited.replacements, "matched": how }),
                None => json!({ "replacements": edited.replacements }),
            },
            is_error: false,
            attachments: Vec::new(),
        })
    }
}

/// The pure edit logic, isolated from any file IO so every edge case
/// (no match, ambiguous match, replace_all) is testable without touching
/// disk.
fn apply_edit(
    original: &str,
    find: &str,
    replace: &str,
    replace_all: bool,
) -> Result<Edited, ToolError> {
    if find.is_empty() {
        return Err(ToolError::Validation("find must not be empty".to_string()));
    }

    // The model's text rarely carries the file's line endings (a ranged
    // `read_file` returns lines joined with `\n`), so in a file that uses one
    // style throughout, both sides are converted to it. A file with mixed
    // endings is matched as given.
    let (find, replace) = match LineEnding::of(original) {
        Some(ending) => (ending.apply(find), ending.apply(replace)),
        None => (find.to_string(), replace.to_string()),
    };
    let (find, replace) = (find.as_str(), replace.as_str());
    if find == replace {
        return Err(ToolError::Validation(
            "find and replace are identical, so the edit would change nothing".to_string(),
        ));
    }

    let occurrences = original.matches(find).count();
    if occurrences == 0 {
        if !replace_all && let Some(loose) = loose_edit(original, find, replace)? {
            return Ok(loose);
        }
        return Err(ToolError::Validation(format!(
            "find string {find:?} was not found in the file"
        )));
    }
    if occurrences > 1 && !replace_all {
        return Err(ToolError::Validation(format!(
            "find string {find:?} matches {occurrences} times; pass replace_all: true or narrow the match"
        )));
    }

    let updated = if replace_all {
        original.replace(find, replace)
    } else {
        original.replacen(find, replace, 1)
    };
    Ok(Edited {
        text: updated,
        replacements: occurrences,
        loose: None,
    })
}

/// The result of an edit.
#[derive(Debug)]
struct Edited {
    text: String,
    replacements: usize,
    /// Set when `find` only matched loosely: how.
    loose: Option<&'static str>,
}

/// How `find` may match when it doesn't match exactly: whole lines,
/// compared with less and less whitespace (models often get indentation or
/// trailing spaces wrong when they copy code).
#[derive(Debug, Clone, Copy)]
enum Loose {
    TrailingWhitespace,
    Indentation,
}

impl Loose {
    fn normalize(self, line: &str) -> &str {
        match self {
            Self::TrailingWhitespace => line.trim_end(),
            Self::Indentation => line.trim(),
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Self::TrailingWhitespace => "ignoring trailing whitespace",
            Self::Indentation => "ignoring indentation; the replacement was re-indented to match",
        }
    }
}

/// Tries the [`Loose`] matches in order. The first that matches anything
/// decides, and must match exactly once — never a guess between places.
/// The matched lines are replaced (keeping the last one's line ending).
fn loose_edit(original: &str, find: &str, replace: &str) -> Result<Option<Edited>, ToolError> {
    let find = find.replace("\r\n", "\n");
    let (find, trailing_newline) = match find.strip_suffix('\n') {
        Some(body) => (body, true),
        None => (find.as_str(), false),
    };
    let find_lines: Vec<&str> = find.split('\n').collect();
    let Some(first_text) = find_lines.iter().position(|l| !l.trim().is_empty()) else {
        return Ok(None);
    };

    // Each line's start offset and its text without the line ending.
    let mut starts = Vec::new();
    let mut lines = Vec::new();
    let mut offset = 0;
    for piece in original.split_inclusive('\n') {
        starts.push(offset);
        let text = piece.strip_suffix('\n').unwrap_or(piece);
        lines.push(text.strip_suffix('\r').unwrap_or(text));
        offset += piece.len();
    }
    let n = find_lines.len();
    if n > lines.len() {
        return Ok(None);
    }

    for loose in [Loose::TrailingWhitespace, Loose::Indentation] {
        let matches: Vec<usize> = (0..=lines.len() - n)
            .filter(|&i| {
                find_lines
                    .iter()
                    .enumerate()
                    .all(|(k, f)| loose.normalize(lines[i + k]) == loose.normalize(f))
            })
            .collect();
        let at = match matches.as_slice() {
            [] => continue,
            [at] => *at,
            many => {
                return Err(ToolError::Validation(format!(
                    "find string {find:?} doesn't match exactly, and matches {} places {}; include more surrounding lines",
                    many.len(),
                    loose.describe().split(';').next().unwrap_or_default()
                )));
            }
        };

        let mut replacement = replace.replace("\r\n", "\n");
        if trailing_newline && replacement.ends_with('\n') {
            replacement.pop();
        }
        if let Loose::Indentation = loose {
            replacement = reindent(
                &replacement,
                indentation(find_lines[first_text]),
                indentation(lines[at + first_text]),
            );
        }
        if let Some(ending) = LineEnding::of(original) {
            replacement = ending.apply(&replacement);
        }
        let start = starts[at];
        let end = starts[at + n - 1] + lines[at + n - 1].len();
        return Ok(Some(Edited {
            text: format!("{}{replacement}{}", &original[..start], &original[end..]),
            replacements: 1,
            loose: Some(loose.describe()),
        }));
    }
    Ok(None)
}

fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Moves `text`'s lines from indentation `from` to `to`: lines starting
/// with `from` get `to` instead; blank lines and lines indented less than
/// `from` are left alone.
fn reindent(text: &str, from: &str, to: &str) -> String {
    text.split('\n')
        .map(|line| match line.strip_prefix(from) {
            Some(rest) if !line.trim().is_empty() => format!("{to}{rest}"),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A file's line-ending style, when it has exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    /// `None` for mixed endings. A file with no line breaks counts as `Lf`,
    /// which leaves single-line text unchanged.
    fn of(text: &str) -> Option<Self> {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count();
        match crlf {
            0 => Some(Self::Lf),
            n if n == lf => Some(Self::Crlf),
            _ => None,
        }
    }

    fn apply(self, text: &str) -> String {
        let lf = text.replace("\r\n", "\n");
        match self {
            Self::Lf => lf,
            Self::Crlf => lf.replace('\n', "\r\n"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecuteWithDefaultContext;
    use arbe_core::{RiskLevel, ToolCallId, TurnId};
    use tempfile::tempdir;

    fn invocation(args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: "edit_file".to_string(),
            arguments: args,
            risk: RiskLevel::Medium,
            rationale: None,
        }
    }

    #[test]
    fn apply_edit_replaces_a_single_match() {
        let Edited {
            text: updated,
            replacements: count,
            ..
        } = apply_edit("hello world", "world", "there", false).unwrap();
        assert_eq!(updated, "hello there");
        assert_eq!(count, 1);
    }

    #[test]
    fn apply_edit_rejects_no_match() {
        assert!(apply_edit("hello world", "xyz", "there", false).is_err());
    }

    #[test]
    fn apply_edit_rejects_an_ambiguous_match_without_replace_all() {
        let err = apply_edit("a a a", "a", "b", false).unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[test]
    fn apply_edit_replaces_every_occurrence_with_replace_all() {
        let Edited {
            text: updated,
            replacements: count,
            ..
        } = apply_edit("a a a", "a", "b", true).unwrap();
        assert_eq!(updated, "b b b");
        assert_eq!(count, 3);
    }

    #[test]
    fn apply_edit_rejects_an_empty_find_string() {
        assert!(apply_edit("hello", "", "x", false).is_err());
    }

    #[test]
    fn apply_edit_rejects_an_edit_that_changes_nothing() {
        let err = apply_edit("hello", "hello", "hello", false).unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
        // Identical once line endings are matched to the file's.
        assert!(apply_edit("a\r\nb", "a\nb", "a\r\nb", false).is_err());
    }

    #[test]
    fn apply_edit_matches_lf_text_in_a_crlf_file_and_keeps_crlf() {
        let original = "fn main() {\r\n    old();\r\n}\r\n";
        let Edited {
            text: updated,
            replacements: count,
            ..
        } = apply_edit(
            original,
            "{\n    old();\n}",
            "{\n    new();\n    more();\n}",
            false,
        )
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(updated, "fn main() {\r\n    new();\r\n    more();\r\n}\r\n");
    }

    #[test]
    fn apply_edit_matches_crlf_text_in_an_lf_file_and_keeps_lf() {
        let Edited { text: updated, .. } =
            apply_edit("a\nb\nc\n", "a\r\nb", "x\r\ny", false).unwrap();
        assert_eq!(updated, "x\ny\nc\n");
    }

    #[test]
    fn apply_edit_matches_a_mixed_ending_file_as_given() {
        let original = "a\r\nb\nc";
        let exact = apply_edit(original, "b\nc", "y", false).unwrap();
        assert_eq!(exact.text, "a\r\ny");
        assert_eq!(exact.loose, None);
        // Not an exact match, but the line-by-line fallback finds it.
        let loose = apply_edit(original, "a\nb", "x", false).unwrap();
        assert_eq!(loose.text, "x\nc");
        assert!(loose.loose.is_some());
    }

    #[test]
    fn a_find_with_wrong_trailing_whitespace_matches_whole_lines() {
        let original = "fn a() {   \n    one();\n}\n";
        let edited = apply_edit(
            original,
            "fn a() {\n    one();",
            "fn a() {\n    two();",
            false,
        )
        .unwrap();
        assert_eq!(edited.text, "fn a() {\n    two();\n}\n");
        assert_eq!(edited.loose, Some("ignoring trailing whitespace"));
    }

    #[test]
    fn a_find_with_wrong_indentation_is_reindented_to_the_file() {
        let original = "impl X {\r\n    fn a() {\r\n        one();\r\n    }\r\n}\r\n";
        // The model dropped the indentation, and added a trailing newline.
        let edited = apply_edit(
            original,
            "fn a() {\n    one();\n}\n",
            "fn a() {\n    one();\n    two();\n}\n",
            false,
        )
        .unwrap();
        assert_eq!(
            edited.text,
            "impl X {\r\n    fn a() {\r\n        one();\r\n        two();\r\n    }\r\n}\r\n"
        );
        assert!(edited.loose.unwrap().starts_with("ignoring indentation"));
    }

    #[test]
    fn a_loose_match_must_be_unique() {
        let original = "if x {\n    go();\n}\nif y {\n  go();\n}\n";
        let err = apply_edit(original, "go();", "stop();", false).unwrap_err();
        // Exactly "go();" is in the file twice already: the exact rule.
        assert!(err.to_string().contains("matches 2 times"), "{err}");
        let err = apply_edit(original, "\tgo();", "stop();", false).unwrap_err();
        assert!(err.to_string().contains("matches 2 places"), "{err}");
        // Never loosened for replace_all.
        assert!(apply_edit("  a  \n", "a\n", "b", true).is_err());
    }

    #[test]
    fn line_ending_of_a_file() {
        assert_eq!(LineEnding::of("one line"), Some(LineEnding::Lf));
        assert_eq!(LineEnding::of("a\nb\n"), Some(LineEnding::Lf));
        assert_eq!(LineEnding::of("a\r\nb\r\n"), Some(LineEnding::Crlf));
        assert_eq!(LineEnding::of("a\r\nb\n"), None);
    }

    #[tokio::test]
    async fn edits_a_real_file_on_disk() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "foo bar").unwrap();
        let tool = EditFileTool::new(dir.path().to_path_buf());

        let result = tool
            .execute_default(invocation(
                json!({ "path": "f.txt", "find": "bar", "replace": "baz" }),
            ))
            .await
            .unwrap();

        assert_eq!(result.output["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "foo baz"
        );
    }

    #[tokio::test]
    async fn rejects_a_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = EditFileTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(
                json!({ "path": "../f.txt", "find": "a", "replace": "b" }),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
