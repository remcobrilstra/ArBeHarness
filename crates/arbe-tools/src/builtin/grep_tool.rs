use std::path::{Path, PathBuf};

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use walkdir::WalkDir;

use super::path_guard::{resolve_within_root, verify_no_symlink_escape};
use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Caps how many matching lines a single `grep` call returns.
const MAX_MATCHES: usize = 500;
/// Files larger than this are skipped rather than scanned — avoids one
/// huge file (a lockfile, a bundled asset) dominating a search.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Same noisy-directory skip list `glob` uses.
const SKIPPED_DIR_NAMES: &[&str] = &[".git", "target", "node_modules", ".venv"];

#[derive(Debug, Deserialize, JsonSchema)]
struct Args {
    /// Regular expression.
    pattern: String,
    /// Directory to search under. Defaults to ".".
    #[serde(default = "default_path")]
    path: String,
    /// Match regardless of case.
    #[serde(default)]
    case_insensitive: bool,
}

fn default_path() -> String {
    ".".to_string()
}

#[derive(Debug, Serialize)]
struct Match {
    file: String,
    line_number: u64,
    line: String,
}

pub struct GrepTool {
    root: PathBuf,
}

impl GrepTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for GrepTool {
    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Search file contents by regex within the project directory, returning matching lines.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid grep arguments: {e}")))?;
        let search_root = resolve_within_root(&self.root, &args.path)?;
        verify_no_symlink_escape(&self.root, &search_root).await?;
        let regex = RegexBuilder::new(&args.pattern)
            .case_insensitive(args.case_insensitive)
            .build()
            .map_err(|e| {
                ToolError::Validation(format!("invalid grep pattern {:?}: {e}", args.pattern))
            })?;

        let search_root_owned = search_root.clone();
        let (matches, truncated) =
            tokio::task::spawn_blocking(move || walk_and_search(&search_root_owned, &regex))
                .await
                .map_err(|e| ToolError::RuntimeFailure(format!("grep task panicked: {e}")))?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "matches": matches, "truncated": truncated }),
            is_error: false,
        })
    }
}

/// The blocking walk + per-line search, isolated from the tool's async
/// plumbing so it's testable as a plain synchronous function.
fn walk_and_search(search_root: &Path, regex: &Regex) -> (Vec<Match>, bool) {
    let mut matches = Vec::new();
    let mut truncated = false;

    let walker = WalkDir::new(search_root).into_iter().filter_entry(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| !SKIPPED_DIR_NAMES.contains(&name))
            .unwrap_or(true)
    });

    'files: for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        // Non-UTF-8-readable files are treated as binary and skipped,
        // same as most line-oriented grep implementations.
        let Ok(contents) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let relative = entry
            .path()
            .strip_prefix(search_root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();

        for (i, line) in contents.lines().enumerate() {
            if regex.is_match(line) {
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'files;
                }
                matches.push(Match {
                    file: relative.clone(),
                    line_number: (i + 1) as u64,
                    line: line.to_string(),
                });
            }
        }
    }

    (matches, truncated)
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
            tool_name: "grep".to_string(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    fn regex(pattern: &str, case_insensitive: bool) -> Regex {
        RegexBuilder::new(pattern)
            .case_insensitive(case_insensitive)
            .build()
            .unwrap()
    }

    #[test]
    fn finds_matching_lines_with_line_numbers() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "alpha\nbeta needle\ngamma").unwrap();

        let (matches, truncated) = walk_and_search(dir.path(), &regex("needle", false));

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].line, "beta needle");
        assert!(!truncated);
    }

    #[test]
    fn is_case_sensitive_by_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "Needle").unwrap();

        let (matches, _) = walk_and_search(dir.path(), &regex("needle", false));
        assert!(matches.is_empty());
    }

    #[test]
    fn case_insensitive_flag_matches_regardless_of_case() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "Needle").unwrap();

        let (matches, _) = walk_and_search(dir.path(), &regex("needle", true));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn searches_across_nested_directories() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/f.txt"), "needle here").unwrap();

        let (matches, _) = walk_and_search(dir.path(), &regex("needle", false));
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn skips_conventionally_noisy_directories() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules/f.txt"), "needle").unwrap();

        let (matches, _) = walk_and_search(dir.path(), &regex("needle", false));
        assert!(matches.is_empty());
    }

    #[test]
    fn skips_files_that_are_not_valid_utf8() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("binary.bin"), [0xFF, 0xFE, 0x00, 0xFF]).unwrap();

        // Should not panic or error, just find nothing.
        let (matches, _) = walk_and_search(dir.path(), &regex("anything", false));
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn invalid_regex_is_a_validation_error() {
        let dir = tempdir().unwrap();
        let tool = GrepTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "pattern": "(unclosed" })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn rejects_a_search_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = GrepTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "pattern": "x", "path": ".." })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
