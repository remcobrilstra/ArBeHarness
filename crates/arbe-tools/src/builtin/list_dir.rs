use std::path::PathBuf;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::path_guard::{resolve_within_root, verify_no_symlink_escape};
use crate::{ToolContext, ToolExecutor};

/// Caps how many entries a single `list_dir` call returns, so a huge
/// directory can't flood the model's context in one call.
const MAX_ENTRIES: usize = 1_000;

#[derive(Debug, Deserialize)]
struct Args {
    #[serde(default = "default_path")]
    path: String,
}

fn default_path() -> String {
    ".".to_string()
}

#[derive(Debug, Serialize)]
struct Entry {
    name: String,
    is_dir: bool,
}

pub struct ListDirTool {
    root: PathBuf,
}

impl ListDirTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for ListDirTool {
    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid list_dir arguments: {e}")))?;
        let path = resolve_within_root(&self.root, &args.path)?;
        verify_no_symlink_escape(&self.root, &path).await?;

        let mut read_dir = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;

        let mut entries = Vec::new();
        let mut truncated = false;
        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?
        {
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "entries": entries, "truncated": truncated }),
            is_error: false,
        })
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
            tool_name: "list_dir".to_string(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[tokio::test]
    async fn lists_files_and_directories_sorted_by_name() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("a_dir")).unwrap();
        std::fs::write(dir.path().join("c.txt"), "").unwrap();

        let tool = ListDirTool::new(dir.path().to_path_buf());
        let result = tool.execute_default(invocation(json!({}))).await.unwrap();

        let entries = result.output["entries"].as_array().unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["a_dir", "b.txt", "c.txt"]);
        assert_eq!(entries[0]["is_dir"], true);
        assert_eq!(entries[1]["is_dir"], false);
    }

    #[tokio::test]
    async fn defaults_to_the_root_when_no_path_given() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "").unwrap();

        let tool = ListDirTool::new(dir.path().to_path_buf());
        let result = tool.execute_default(invocation(json!({}))).await.unwrap();

        assert_eq!(result.output["entries"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn lists_a_subdirectory() {
        let dir = tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/inner.txt"), "").unwrap();

        let tool = ListDirTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(json!({ "path": "sub" })))
            .await
            .unwrap();

        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries[0]["name"], "inner.txt");
    }

    #[tokio::test]
    async fn rejects_a_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = ListDirTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "path": ".." })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn missing_directory_is_a_runtime_failure() {
        let dir = tempdir().unwrap();
        let tool = ListDirTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "path": "nope" })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::RuntimeFailure(_)));
    }
}
