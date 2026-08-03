use std::path::{Path, PathBuf};

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;
use serde_json::json;
use walkdir::WalkDir;

use super::path_guard::resolve_within_root;
use crate::ToolExecutor;

/// Caps how many matches a single `glob` call returns, so a broad pattern
/// over a large tree can't flood the model's context in one call.
const MAX_MATCHES: usize = 2_000;

/// Directories skipped entirely during the walk — noisy, usually huge, and
/// never what a glob over source code is looking for.
const SKIPPED_DIR_NAMES: &[&str] = &[".git", "target", "node_modules", ".venv"];

#[derive(Debug, Deserialize)]
struct Args {
    /// e.g. `"**/*.rs"`, `"src/*.toml"`.
    pattern: String,
    #[serde(default = "default_path")]
    path: String,
}

fn default_path() -> String {
    ".".to_string()
}

pub struct GlobTool {
    root: PathBuf,
}

impl GlobTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for GlobTool {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid glob arguments: {e}")))?;
        let search_root = resolve_within_root(&self.root, &args.path)?;
        let matcher = build_matcher(&args.pattern)?;

        let search_root_owned = search_root.clone();
        let (matches, truncated) =
            tokio::task::spawn_blocking(move || walk_and_match(&search_root_owned, &matcher))
                .await
                .map_err(|e| ToolError::RuntimeFailure(format!("glob task panicked: {e}")))?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "matches": matches, "truncated": truncated }),
            is_error: false,
        })
    }
}

/// `literal_separator(true)` makes a single `*` stop at a path separator
/// (so `*.rs` only matches top-level files) while `**` still crosses
/// directory boundaries — the intuitive split most glob users expect, and
/// the one `walk_and_match`'s tests rely on.
fn build_matcher(pattern: &str) -> Result<GlobMatcher, ToolError> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher())
        .map_err(|e| ToolError::Validation(format!("invalid glob pattern {pattern:?}: {e}")))
}

/// The blocking directory walk + match, isolated from the tool's async
/// plumbing so it's testable as a plain synchronous function.
fn walk_and_match(search_root: &Path, matcher: &GlobMatcher) -> (Vec<String>, bool) {
    let mut matches = Vec::new();
    let mut truncated = false;

    let walker = WalkDir::new(search_root).into_iter().filter_entry(|entry| {
        entry
            .file_name()
            .to_str()
            .map(|name| !SKIPPED_DIR_NAMES.contains(&name))
            .unwrap_or(true)
    });

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(search_root) else {
            continue;
        };
        if matcher.is_match(relative) {
            if matches.len() >= MAX_MATCHES {
                truncated = true;
                break;
            }
            matches.push(relative.to_string_lossy().into_owned());
        }
    }

    matches.sort();
    (matches, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::{RiskLevel, ToolCallId, TurnId};
    use tempfile::tempdir;

    fn invocation(args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: "glob".to_string(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[test]
    fn matches_files_at_any_depth_with_double_star() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.rs"), "").unwrap();
        std::fs::write(dir.path().join("c.toml"), "").unwrap();

        let matcher = build_matcher("**/*.rs").unwrap();
        let (matches, truncated) = walk_and_match(dir.path(), &matcher);

        let expected_nested = PathBuf::from("sub")
            .join("b.rs")
            .to_string_lossy()
            .into_owned();
        assert_eq!(matches, vec!["a.rs".to_string(), expected_nested]);
        assert!(!truncated);
    }

    #[test]
    fn single_star_does_not_cross_directory_boundaries() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.rs"), "").unwrap();

        let matcher = build_matcher("*.rs").unwrap();
        let (matches, _) = walk_and_match(dir.path(), &matcher);

        assert_eq!(matches, vec!["a.rs"]);
    }

    #[test]
    fn skips_conventionally_noisy_directories() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/built.rs"), "").unwrap();
        std::fs::write(dir.path().join("real.rs"), "").unwrap();

        let matcher = build_matcher("**/*.rs").unwrap();
        let (matches, _) = walk_and_match(dir.path(), &matcher);

        assert_eq!(matches, vec!["real.rs"]);
    }

    #[test]
    fn invalid_pattern_is_a_validation_error() {
        let err = build_matcher("[").unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn end_to_end_through_the_tool_executor() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        let tool = GlobTool::new(dir.path().to_path_buf());

        let result = tool
            .execute(invocation(json!({ "pattern": "*.rs" })))
            .await
            .unwrap();

        assert_eq!(result.output["matches"], json!(["main.rs"]));
    }

    #[tokio::test]
    async fn rejects_a_search_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = GlobTool::new(dir.path().to_path_buf());
        let err = tool
            .execute(invocation(json!({ "pattern": "*", "path": ".." })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
