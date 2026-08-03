use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::ToolExecutor;

/// Default and maximum allowed `timeout_secs` — a command with no timeout
/// still needs *some* bound so a runaway process can't hang the loop
/// forever.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Deserialize)]
struct Args {
    command: String,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// Runs a shell command with the agent's project directory as its working
/// directory. **This is the highest-risk builtin tool** — arbitrary
/// command execution can't be meaningfully sandboxed beyond scoping the
/// working directory, which is exactly why every invocation must pass
/// through the approval gate (`arbe_tools::execute_gated`) same as any
/// other tool; nothing about this executor bypasses that.
pub struct ExecuteTool {
    root: PathBuf,
}

impl ExecuteTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for ExecuteTool {
    async fn execute(&self, invocation: ToolInvocation) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid execute arguments: {e}")))?;
        let timeout_secs = args
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        let mut command = shell_command(&args.command);
        command
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = command
            .spawn()
            .map_err(|e| ToolError::RuntimeFailure(format!("failed to spawn command: {e}")))?;

        let output =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                .await
                .map_err(|_| ToolError::Timeout)?
                .map_err(|e| ToolError::RuntimeFailure(format!("command execution failed: {e}")))?;

        let exit_code = output.status.code();
        let is_error = exit_code != Some(0);

        Ok(ToolResult {
            id: invocation.id,
            output: json!({
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
                "exit_code": exit_code,
            }),
            is_error,
        })
    }
}

/// Wraps `command` in the platform shell, so callers can use pipes,
/// redirection, and multiple statements the same way they would at a
/// terminal, rather than being limited to a single argv-style program +
/// args.
fn shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
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
            tool_name: "execute".to_string(),
            arguments: args,
            risk: RiskLevel::High,
            rationale: None,
        }
    }

    #[tokio::test]
    async fn runs_a_command_and_captures_stdout() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());

        let result = tool
            .execute(invocation(json!({ "command": "echo hello" })))
            .await
            .unwrap();

        assert_eq!(result.output["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(result.output["exit_code"], 0);
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_reported_not_treated_as_a_tool_error() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());

        let result = tool
            .execute(invocation(json!({ "command": "exit 3" })))
            .await
            .unwrap();

        assert_eq!(result.output["exit_code"], 3);
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn runs_with_the_configured_directory_as_cwd() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "").unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());

        let list_command = if cfg!(windows) { "dir /b" } else { "ls" };
        let result = tool
            .execute(invocation(json!({ "command": list_command })))
            .await
            .unwrap();

        assert!(
            result.output["stdout"]
                .as_str()
                .unwrap()
                .contains("marker.txt")
        );
    }

    #[tokio::test]
    async fn a_slow_command_past_the_timeout_is_a_timeout_error() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        // Avoid nested quoting through `cmd /C` (unreliable — Rust's argv
        // escaping and cmd's own quote-stripping don't compose cleanly);
        // `ping` is the standard quote-free way to sleep N-1 seconds on
        // Windows without depending on PowerShell being on PATH.
        let sleep_command = if cfg!(windows) {
            "ping -n 6 127.0.0.1"
        } else {
            "sleep 5"
        };

        let err = tool
            .execute(invocation(
                json!({ "command": sleep_command, "timeout_secs": 1 }),
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, ToolError::Timeout));
    }

    #[tokio::test]
    async fn invalid_arguments_are_a_validation_error() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        let err = tool.execute(invocation(json!({}))).await.unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
