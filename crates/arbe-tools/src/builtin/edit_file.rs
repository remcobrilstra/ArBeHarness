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
            "Find-and-replace a substring within an existing file. Fails if `find` doesn't match, or matches more than once unless replace_all is set.",
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

        let (updated, replacements) =
            apply_edit(&original, &args.find, &args.replace, args.replace_all)?;

        let write_path = path.clone();
        tokio::task::spawn_blocking(move || write_atomic(&write_path, updated.as_bytes()))
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("write task panicked: {e}")))?
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "replacements": replacements }),
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
) -> Result<(String, usize), ToolError> {
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
    Ok((updated, occurrences))
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
        let (updated, count) = apply_edit("hello world", "world", "there", false).unwrap();
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
        let (updated, count) = apply_edit("a a a", "a", "b", true).unwrap();
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
        let (updated, count) = apply_edit(
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
        let (updated, _) = apply_edit("a\nb\nc\n", "a\r\nb", "x\r\ny", false).unwrap();
        assert_eq!(updated, "x\ny\nc\n");
    }

    #[test]
    fn apply_edit_matches_a_mixed_ending_file_as_given() {
        let original = "a\r\nb\nc";
        assert!(apply_edit(original, "a\nb", "x", false).is_err());
        let (updated, _) = apply_edit(original, "b\nc", "y", false).unwrap();
        assert_eq!(updated, "a\r\ny");
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
