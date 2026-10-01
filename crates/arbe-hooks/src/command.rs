//! Hooks that run a shell command.
//!
//! The command gets the phase's JSON payload on stdin, with a `"phase"`
//! field added. What it prints decides the result:
//! - nothing: the payload passes through unchanged;
//! - a JSON object: it replaces the payload (e.g. `before_tool_execute`
//!   can change `arguments` or add `"veto": "reason"`);
//! - anything else, or a non-zero exit status: the hook failed (its stderr
//!   is included in the failure report). A failed hook is skipped, unless
//!   it was set to block on failure ([`CommandHook::blocking_on_failure`]).
//!
//! The payload is written to stdin while stdout is being read, so a hook
//! that echoes as it reads can't deadlock on a payload bigger than the
//! pipe buffer.

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
    block_on_failure: bool,
}

impl CommandHook {
    pub fn new(phase: HookPhase, command: impl Into<String>) -> Self {
        Self {
            phase,
            command: command.into(),
            cwd: None,
            timeout: DEFAULT_COMMAND_TIMEOUT,
            block_on_failure: false,
        }
    }

    /// Whether a failure of this hook (error, bad output, timeout) should
    /// block what it guards instead of being skipped — for a
    /// `before_tool_execute` guard, a failure then refuses the call.
    pub fn blocking_on_failure(mut self, block: bool) -> Self {
        self.block_on_failure = block;
        self
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

/// The platform shell running `command` as one command line (see
/// `arbe_core::shell::command`).
fn shell(command: &str) -> Command {
    Command::from(arbe_core::shell::command(command))
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

    fn blocks_on_failure(&self) -> bool {
        self.block_on_failure
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
        let stdin = child.stdin.take();
        let text = input.to_string();
        // Written while the output is read: a hook that prints as it reads
        // would otherwise fill its stdout pipe and stop reading stdin
        // while we're still writing to it.
        let feed = async move {
            if let Some(mut stdin) = stdin {
                // A command that doesn't read its input may close stdin
                // early; that's fine. Dropping `stdin` closes it.
                let _ = stdin.write_all(text.as_bytes()).await;
            }
        };
        let (_, output) = tokio::join!(feed, child.wait_with_output());
        let output = output.map_err(|e| HookError::ContractViolation(e.to_string()))?;

        if !output.status.success() {
            // Both streams: a check command (compiler, test runner) may
            // report what failed on either.
            let printed: Vec<String> = [&output.stderr, &output.stdout]
                .into_iter()
                .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
                .filter(|text| !text.is_empty())
                .collect();
            return Err(HookError::ContractViolation(format!(
                "exited with {}{}",
                output.status,
                if printed.is_empty() {
                    String::new()
                } else {
                    format!(": {}", printed.join("\n"))
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
    async fn a_hook_that_echoes_a_large_payload_does_not_deadlock() {
        // Far bigger than any pipe buffer: the hook starts printing before
        // it has read everything, so stdin must be fed while stdout drains.
        let echo = if cfg!(windows) { "more" } else { "cat" };
        let big = "x".repeat(512 * 1024);
        let hook = CommandHook::new(HookPhase::BeforeToolExecute, echo);
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            hook.run(json!({"arguments": {"content": big}})),
        )
        .await
        .expect("the hook deadlocked");
        // `cat` echoes verbatim; Windows' `more` reflows long lines, so
        // there only finishing counts.
        if cfg!(unix) {
            let result = result.unwrap();
            assert_eq!(
                result["arguments"]["content"].as_str().unwrap().len(),
                big.len()
            );
        }
    }

    #[test]
    fn blocking_on_failure_is_opt_in() {
        let hook = CommandHook::new(HookPhase::BeforeToolExecute, "exit 1");
        assert!(!hook.blocks_on_failure());
        assert!(hook.blocking_on_failure(true).blocks_on_failure());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_a_failure_with_stderr() {
        let hook = CommandHook::new(HookPhase::OnError, "echo nope 1>&2 && exit 3");
        let err = hook.run(payload()).await.unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
    }

    #[tokio::test]
    async fn a_non_zero_exit_reports_stdout_too() {
        // Test runners print their failures to stdout.
        let hook = CommandHook::new(HookPhase::BeforeTurnEnd, "echo 1 failed && exit 1");
        let err = hook.run(payload()).await.unwrap_err().to_string();
        assert!(err.contains("1 failed"), "{err}");
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
