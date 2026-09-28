use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use arbe_core::{ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::{ToolContext, ToolExecutor};

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
    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
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
        // Own process group, so the whole tree (the shell *and* whatever it
        // started) can be killed together — see `kill_process_tree`.
        #[cfg(unix)]
        command.process_group(0);

        let child = command
            .spawn()
            .map_err(|e| ToolError::RuntimeFailure(format!("failed to spawn command: {e}")))?;
        let pid = child.id();

        // Pinned outside the `select!` so the child stays alive (and
        // findable) until its tree is killed below; dropping `wait` first
        // would kill only the shell and orphan its children.
        let mut wait = Box::pin(tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            child.wait_with_output(),
        ));
        let outcome = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
            output = &mut wait => match output {
                Err(_elapsed) => Err(ToolError::Timeout),
                Ok(result) => result.map_err(|e| {
                    ToolError::RuntimeFailure(format!("command execution failed: {e}"))
                }),
            },
        };
        if outcome.is_err()
            && let Some(pid) = pid
        {
            kill_process_tree(pid).await;
        }
        drop(wait);
        let output = outcome?;

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

/// Kills a command's whole process tree. Killing just the direct child
/// (the shell) isn't enough: on Windows `cmd /C` never replaces itself, so
/// the real program survives as an orphan, and on Unix any compound
/// command (`a && b`, pipelines) does the same. A survivor keeps running
/// after a timeout/cancel and holds the output pipes open. Best effort —
/// failures are ignored, since the tree may already have exited.
async fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    let mut killer = {
        let mut c = Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid.to_string()]);
        c
    };
    #[cfg(unix)]
    let mut killer = {
        // The child leads its own process group (`process_group(0)` at
        // spawn), so its pgid is its pid; a negative target kills the group.
        let mut c = Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    let _ = killer
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
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
    use crate::ExecuteWithDefaultContext;
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
            .execute_default(invocation(json!({ "command": "echo hello" })))
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
            .execute_default(invocation(json!({ "command": "exit 3" })))
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
            .execute_default(invocation(json!({ "command": list_command })))
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
            .execute_default(invocation(
                json!({ "command": sleep_command, "timeout_secs": 1 }),
            ))
            .await
            .unwrap_err();

        assert!(matches!(err, ToolError::Timeout));
    }

    #[tokio::test]
    async fn cancelling_the_context_stops_a_running_command_promptly() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        let sleep_command = if cfg!(windows) {
            "ping -n 31 127.0.0.1"
        } else {
            "sleep 30"
        };
        let ctx = ToolContext::default();
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });

        let started = std::time::Instant::now();
        let err = tool
            .execute(
                invocation(json!({ "command": sleep_command, "timeout_secs": 60 })),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(matches!(err, ToolError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn invalid_arguments_are_a_validation_error() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }
}
