//! Hooks that run a shell command.
//!
//! The command gets the phase's JSON payload on stdin, with a `"phase"`
//! field added. What it prints decides the result:
//! - nothing: the payload passes through unchanged;
//! - a JSON object: it replaces the payload (e.g. `before_tool_execute`
//!   can change `arguments` or add `"veto": "reason"`);
//! - anything else, or a non-zero exit status: the hook failed and is
//!   skipped (its stderr is included in the failure report).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use arbe_core::HookError;
use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::{Hook, HookPhase};

/// Default time limit for a command hook (process start-up included).
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub struct CommandHook {
    phase: HookPhase,
    command: String,
    cwd: Option<PathBuf>,
    timeout: Duration,
}

impl CommandHook {
    pub fn new(phase: HookPhase, command: impl Into<String>) -> Self {
        Self {
            phase,
            command: command.into(),
            cwd: None,
            timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }

    /// Runs the command in `cwd` (the project directory, usually).
    pub fn in_dir(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// The platform shell running `command` as one command line. On Windows
/// the line goes to `cmd` verbatim (`raw_arg`), since Rust's argument
/// quoting escapes inner quotes in a way `cmd` doesn't understand (same
/// approach as `arbe_tools::builtin::execute_tool::shell_command`).
fn shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/S /C \"{command}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
}

#[async_trait]
impl Hook for CommandHook {
    fn phase(&self) -> HookPhase {
        self.phase
    }

    fn name(&self) -> String {
        format!("{} hook `{}`", self.phase.name(), self.command)
    }

    fn timeout(&self) -> Option<Duration> {
        Some(self.timeout)
    }

    async fn run(&self, payload: Value) -> Result<Value, HookError> {
        let mut input = payload.clone();
        if let Value::Object(map) = &mut input {
            map.insert("phase".into(), Value::String(self.phase.name().into()));
        }

        let mut command = shell(&self.command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A timed-out hook's task is aborted; this makes that kill the
            // process too.
            .kill_on_drop(true);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|e| HookError::ContractViolation(format!("could not start: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A command that doesn't read its input may close stdin early;
            // that's fine.
            let _ = stdin.write_all(input.to_string().as_bytes()).await;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| HookError::ContractViolation(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HookError::ContractViolation(format!(
                "exited with {}{}",
                output.status,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            return Ok(payload);
        }
        match serde_json::from_str::<Value>(stdout.trim()) {
            Ok(Value::Object(mut map)) => {
                map.remove("phase");
                Ok(Value::Object(map))
            }
            _ => Err(HookError::ContractViolation(format!(
                "printed something other than a JSON object: {}",
                stdout.trim().chars().take(200).collect::<String>()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({ "tool_name": "execute", "arguments": { "command": "rm -rf /" } })
    }

    /// Prints `json` verbatim, on either platform's shell.
    fn print_json(json: &str) -> String {
        if cfg!(windows) {
            format!("echo {json}")
        } else {
            format!("echo '{json}'")
        }
    }

    #[tokio::test]
    async fn silence_passes_the_payload_through() {
        let hook = CommandHook::new(HookPhase::BeforeToolExecute, "exit 0");
        assert_eq!(hook.run(payload()).await.unwrap(), payload());
    }

    #[tokio::test]
    async fn a_printed_object_replaces_the_payload() {
        let hook = CommandHook::new(
            HookPhase::BeforeToolExecute,
            print_json(r#"{"veto": "no deleting", "phase": "before_tool_execute"}"#),
        );
        assert_eq!(
            hook.run(payload()).await.unwrap(),
            json!({"veto": "no deleting"})
        );
    }

    #[tokio::test]
    async fn the_payload_arrives_on_stdin_with_the_phase() {
        // Echo stdin back: the result is the input minus the added phase.
        let echo = if cfg!(windows) { "more" } else { "cat" };
        let hook = CommandHook::new(HookPhase::AfterModelCall, echo);
        let result = hook.run(json!({"turn_id": "t1"})).await.unwrap();
        assert_eq!(result, json!({"turn_id": "t1"}));
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_failure_with_stderr() {
        let hook = CommandHook::new(HookPhase::OnError, "echo nope 1>&2 && exit 3");
        let err = hook.run(payload()).await.unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[tokio::test]
    async fn non_json_output_is_a_failure() {
        let hook = CommandHook::new(HookPhase::OnError, "echo hello");
        assert!(hook.run(payload()).await.is_err());
    }

    #[tokio::test]
    async fn runs_in_the_given_directory() {
        let dir = std::env::temp_dir();
        let pwd = if cfg!(windows) { "cd" } else { "pwd" };
        // Output isn't JSON, so this fails — with the directory in the
        // message, which is all this checks.
        let hook = CommandHook::new(HookPhase::OnError, pwd).in_dir(dir.clone());
        let err = hook.run(payload()).await.unwrap_err().to_string();
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(err.contains(&name), "{err}");
    }
}
