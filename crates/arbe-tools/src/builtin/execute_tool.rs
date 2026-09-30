use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use super::processes::ProcessTable;
use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Default and maximum allowed `timeout_secs` — a command with no timeout
/// still needs *some* bound so a runaway process can't hang the loop
/// forever.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;

/// Most of each output stream kept in memory: its start and its end, with
/// what's between counted and dropped. A command that prints without end
/// (`yes`, `cat` of a huge file) can't exhaust memory before its timeout.
/// The agent later shortens the result further for the model.
const CAPTURE_HEAD_BYTES: usize = 512 * 1024;
const CAPTURE_TAIL_BYTES: usize = 512 * 1024;

/// One output stream: its first and last bytes, and how many in between
/// were dropped.
#[derive(Default)]
struct Capture {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    dropped: u64,
}

impl Capture {
    fn push(&mut self, mut bytes: &[u8]) {
        let room = CAPTURE_HEAD_BYTES - self.head.len();
        if room > 0 {
            let take = room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend(bytes);
        if self.tail.len() > CAPTURE_TAIL_BYTES {
            let excess = self.tail.len() - CAPTURE_TAIL_BYTES;
            self.tail.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    fn into_text(self) -> String {
        let mut text = String::from_utf8_lossy(&self.head).into_owned();
        if self.dropped > 0 {
            text.push_str(&format!("\n[... {} bytes omitted ...]\n", self.dropped));
        }
        let tail: Vec<u8> = self.tail.into_iter().collect();
        text.push_str(&String::from_utf8_lossy(&tail));
        text
    }
}

/// Reads `stream` to its end into a [`Capture`].
async fn capture(stream: Option<impl tokio::io::AsyncRead + Unpin>) -> Capture {
    use tokio::io::AsyncReadExt;
    let mut captured = Capture::default();
    let Some(mut stream) = stream else {
        return captured;
    };
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => captured.push(&buf[..n]),
        }
    }
    captured
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Args {
    /// The command line to run (via `cmd /C` on Windows, `sh -c` elsewhere).
    command: String,
    /// Seconds before the command is killed. Defaults to 30, capped at 300.
    /// Ignored with `background`.
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Start the command and return at once with a handle, instead of
    /// waiting for it to finish — for servers, watchers and long builds.
    /// Read its output with `process_output`, stop it with `process_kill`.
    #[serde(default)]
    background: bool,
}

/// Runs a shell command with the agent's project directory as its working
/// directory. **This is the highest-risk builtin tool** — arbitrary
/// command execution can't be meaningfully sandboxed beyond scoping the
/// working directory, which is exactly why every invocation must pass
/// through the approval gate (`arbe_tools::execute_gated`) same as any
/// other tool; nothing about this executor bypasses that.
pub struct ExecuteTool {
    root: PathBuf,
    /// Where `background: true` commands go (shared with `process_output`
    /// and `process_kill`).
    processes: Arc<ProcessTable>,
}

impl ExecuteTool {
    /// With a background-process table of its own.
    pub fn new(root: PathBuf) -> Self {
        Self::with_processes(root, Arc::new(ProcessTable::new()))
    }

    pub fn with_processes(root: PathBuf, processes: Arc<ProcessTable>) -> Self {
        Self { root, processes }
    }
}

#[async_trait]
impl ToolExecutor for ExecuteTool {
    /// Rule subject: the command line (see `ToolExecutor::subject`).
    fn subject(&self, arguments: &serde_json::Value) -> Option<String> {
        arguments
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(|c| c.trim().to_string())
    }

    fn subject_kind(&self) -> crate::SubjectKind {
        crate::SubjectKind::ShellCommand
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Run a shell command with the project directory as its working directory and return its output. With `background: true` it keeps running and you get a handle for `process_output` / `process_kill`. Highest-risk tool — always approval-gated.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::High
    }

    /// Not parallel-safe: it runs an arbitrary command.
    fn parallel_safe(&self) -> bool {
        false
    }

    /// Background processes don't outlive their session.
    fn close(&self) {
        self.processes.kill_all();
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid execute arguments: {e}")))?;
        if args.background {
            let (handle, pid) = self.processes.spawn(&args.command, &self.root)?;
            return Ok(ToolResult {
                id: invocation.id,
                output: json!({
                    "handle": handle,
                    "pid": pid,
                    "status": "running in the background",
                    "next": "read its output with process_output; stop it with process_kill",
                }),
                is_error: false,
                attachments: Vec::new(),
            });
        }
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

        let mut child = command
            .spawn()
            .map_err(|e| ToolError::RuntimeFailure(format!("failed to spawn command: {e}")))?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Pinned outside the `select!` so the child stays alive (and
        // findable) until its tree is killed below; dropping `run` first
        // would kill only the shell and orphan its children.
        let mut run = Box::pin(tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            async {
                let (out, err, status) =
                    tokio::join!(capture(stdout), capture(stderr), child.wait());
                status.map(|status| (out, err, status))
            },
        ));
        let outcome = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
            output = &mut run => match output {
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
        drop(run);
        let (stdout, stderr, status) = outcome?;

        let exit_code = status.code();
        let is_error = exit_code != Some(0);

        Ok(ToolResult {
            id: invocation.id,
            output: json!({
                "stdout": stdout.into_text(),
                "stderr": stderr.into_text(),
                "exit_code": exit_code,
            }),
            is_error,
            attachments: Vec::new(),
        })
    }
}

/// Kills a command's whole process tree. Killing just the direct child
/// (the shell) isn't enough: on Windows `cmd /C` never replaces itself, so
/// the real program survives as an orphan, and on Unix any compound
/// command (`a && b`, pipelines) does the same. A survivor keeps running
/// after a timeout/cancel and holds the output pipes open. Best effort —
/// failures are ignored, since the tree may already have exited.
pub(super) async fn kill_process_tree(pid: u32) {
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

/// [`kill_process_tree`] for places that can't await (e.g. `Drop`).
pub(super) fn kill_process_tree_blocking(pid: u32) {
    #[cfg(windows)]
    let mut killer = {
        let mut c = std::process::Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid.to_string()]);
        c
    };
    #[cfg(unix)]
    let mut killer = {
        let mut c = std::process::Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    let _ = killer
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `command` in the platform shell (see `arbe_core::shell::command`), so
/// pipes, redirection and multiple statements work as at a terminal.
pub fn shell_command(command: &str) -> Command {
    Command::from(arbe_core::shell::command(command))
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

    #[test]
    fn captured_output_keeps_its_start_and_end_within_a_fixed_size() {
        let mut capture = Capture::default();
        capture.push(&vec![b'a'; CAPTURE_HEAD_BYTES]);
        for _ in 0..10 {
            capture.push(&vec![b'b'; CAPTURE_TAIL_BYTES]);
        }
        capture.push(b"THE END");
        assert_eq!(capture.head.len(), CAPTURE_HEAD_BYTES);
        assert_eq!(capture.tail.len(), CAPTURE_TAIL_BYTES);
        assert_eq!(capture.dropped, 9 * CAPTURE_TAIL_BYTES as u64 + 7);
        let text = capture.into_text();
        assert!(text.starts_with("aaa"));
        assert!(text.ends_with("bbbTHE END"));
        assert!(text.contains(&format!(
            "[... {} bytes omitted ...]",
            9 * CAPTURE_TAIL_BYTES + 7
        )));

        let mut small = Capture::default();
        small.push(b"hello");
        assert_eq!(small.into_text(), "hello");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_that_prints_without_end_is_captured_within_bounds() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(
                json!({ "command": "head -c 5000000 /dev/zero | tr '\\0' x; echo done" }),
            ))
            .await
            .unwrap();
        let stdout = result.output["stdout"].as_str().unwrap();
        assert!(stdout.len() < CAPTURE_HEAD_BYTES + CAPTURE_TAIL_BYTES + 100);
        assert!(stdout.contains("bytes omitted"));
        assert!(stdout.trim_end().ends_with("done"));
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

    /// Quotes must reach the shell intact on every platform (on Windows,
    /// Rust's default argument escaping used to mangle them for `cmd`).
    #[tokio::test]
    async fn quoted_arguments_reach_the_shell_intact() {
        let dir = tempdir().unwrap();
        let tool = ExecuteTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(
                json!({ "command": "echo \"hello  world\" && echo second" }),
            ))
            .await
            .unwrap();
        let stdout = result.output["stdout"].as_str().unwrap();
        assert!(stdout.contains("hello  world"), "{stdout:?}");
        assert!(stdout.contains("second"), "{stdout:?}");
        assert!(
            !stdout.contains('\\'),
            "backslash escapes leaked: {stdout:?}"
        );
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
        let ctx = ToolContext::for_testing();
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
