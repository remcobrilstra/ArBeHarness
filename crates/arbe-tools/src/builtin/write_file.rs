use std::path::PathBuf;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use arbe_storage::atomic::write_atomic;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::path_guard::{resolve_within_root, verify_no_symlink_escape};
use crate::{ToolContext, ToolDescription, ToolExecutor};

#[derive(Debug, Deserialize, JsonSchema)]
struct Args {
    /// Path relative to the project root.
    path: String,
    /// The complete new file content.
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
    /// Rule subject: the path (see `ToolExecutor::subject`).
    fn subject(&self, arguments: &serde_json::Value) -> Option<String> {
        crate::path_subject(arguments, None)
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Create or overwrite a file within the project directory with the given content.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    /// Not parallel-safe: it writes a file.
    fn parallel_safe(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid write_file arguments: {e}")))?;
        let path = resolve_within_root(&self.root, &args.path)?;
        verify_no_symlink_escape(&self.root, &path).await?;

        let bytes_written = args.content.len();
        tokio::task::spawn_blocking(move || write_atomic(&path, args.content.as_bytes()))
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("write task panicked: {e}")))?
            .map_err(|e| ToolError::RuntimeFailure(e.to_string()))?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "bytes_written": bytes_written }),
            is_error: false,
            attachments: Vec::new(),
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
            .execute_default(invocation(json!({ "path": "new.txt", "content": "hi" })))
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

        tool.execute_default(invocation(json!({ "path": "a/b/c.txt", "content": "x" })))
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

        tool.execute_default(invocation(json!({ "path": "f.txt", "content": "new" })))
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
            .execute_default(invocation(
                json!({ "path": "../escape.txt", "content": "x" }),
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
