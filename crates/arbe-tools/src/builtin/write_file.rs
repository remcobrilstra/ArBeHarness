use std::path::PathBuf;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use arbe_storage::atomic::write_atomic;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::path_guard::resolve_within_root;
use crate::ToolExecutor;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    content: String,
}

/// Creates or overwrites a file. Reuses `arbe_storage::atomic::write_atomic`
/// (temp-file-then-rename) so a crash mid-write can never leave a
/// truncated file behind — the same guarantee session persistence gets.
pub struct WriteFileTool {
    root: PathBuf,
}

impl WriteFileTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for WriteFileTool {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid write_file arguments: {e}")))?;
        let path = resolve_within_root(&self.root, &args.path)?;

        let bytes_written = args.content.len();
        tokio::task::spawn_blocking(move || write_atomic(&path, args.content.as_bytes()))
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("write task panicked: {e}")))?
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "bytes_written": bytes_written }),
            is_error: false,
        })
    }
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
            tool_name: "write_file".to_string(),
            arguments: args,
            risk: RiskLevel::Medium,
            rationale: None,
        }
    }

    #[tokio::test]
    async fn writes_a_new_file() {
        let dir = tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        let result = tool
            .execute(invocation(json!({ "path": "new.txt", "content": "hi" })))
            .await
            .unwrap();

        assert_eq!(result.output["bytes_written"], 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hi"
        );
    }

    #[tokio::test]
    async fn creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        tool.execute(invocation(json!({ "path": "a/b/c.txt", "content": "x" })))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "x"
        );
    }

    #[tokio::test]
    async fn overwrites_an_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old").unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());

        tool.execute(invocation(json!({ "path": "f.txt", "content": "new" })))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn rejects_a_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_path_buf());
        let err = tool
            .execute(invocation(
                json!({ "path": "../escape.txt", "content": "x" }),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
