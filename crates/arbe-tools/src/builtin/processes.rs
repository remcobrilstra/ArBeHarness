//! Background processes (v2 plan P6.4): `execute` with `background: true`
//! starts a command and returns at once with a handle; `process_output`
//! reads what it has printed since the last read (and whether it's still
//! running); `process_kill` stops it. For dev servers, watchers, long
//! builds — anything the agent wants to keep running while it works.
//!
//! The processes belong to the session's [`ProcessTable`], shared by those
//! three tools: when the table is dropped (the session's agent goes away,
//! or the app exits), every process tree still running is killed.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arbe_core::{RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::execute_tool::{kill_process_tree, kill_process_tree_blocking, shell_command};
use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Output kept per process; older output is dropped (and counted).
const MAX_BUFFER_BYTES: usize = 256 * 1024;
/// Most output one `process_output` call returns.
const MAX_READ_BYTES: usize = 32 * 1024;
/// Background processes one session may have at once (running or not yet
/// collected).
const MAX_PROCESSES: usize = 16;
/// Longest `process_output` may wait for new output.
const MAX_WAIT_SECS: u64 = 30;

/// A process's combined stdout and stderr, capped to the newest
/// `MAX_BUFFER_BYTES`, with a read cursor.
#[derive(Default)]
struct Output {
    text: String,
    /// Absolute offset of `text`'s first byte in everything ever printed.
    start: usize,
    /// Absolute offset up to which output has been read.
    read: usize,
}

impl Output {
    fn push(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        if self.text.len() > MAX_BUFFER_BYTES {
            let mut cut = self.text.len() - MAX_BUFFER_BYTES;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.start += cut;
        }
    }

    fn end(&self) -> usize {
        self.start + self.text.len()
    }

    /// Unread output (at most `MAX_READ_BYTES`, the oldest first), and how
    /// many unread bytes were lost to the cap.
    fn read_new(&mut self) -> (String, usize) {
        let dropped = self.start.saturating_sub(self.read);
        let from = self.read.max(self.start) - self.start;
        let mut to = (from + MAX_READ_BYTES).min(self.text.len());
        while !self.text.is_char_boundary(to) {
            to -= 1;
        }
        let chunk = self.text[from..to].to_string();
        self.read = self.start + to;
        (chunk, dropped)
    }
}

struct Process {
    command: String,
    pid: Option<u32>,
    output: Arc<Mutex<Output>>,
    /// `Some(exit code)` once it has exited (`None` inside: killed by a
    /// signal, no code).
    exited: Arc<Mutex<Option<Option<i32>>>>,
    /// Notified on new output and on exit.
    changed: Arc<tokio::sync::Notify>,
}

impl Process {
    fn status(&self) -> Value {
        match *self.exited.lock().unwrap_or_else(|p| p.into_inner()) {
            None => json!({"status": "running"}),
            Some(code) => json!({"status": "exited", "exit_code": code}),
        }
    }

    fn running(&self) -> bool {
        self.exited
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_none()
    }
}

/// A session's background processes, by handle (`bg-1`, `bg-2`, …).
#[derive(Default)]
pub struct ProcessTable {
    inner: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    next: u32,
    processes: BTreeMap<String, Process>,
}

impl ProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts `command` in `root` in the background. Returns its handle and
    /// pid.
    pub(super) fn spawn(
        &self,
        command: &str,
        root: &Path,
    ) -> Result<(String, Option<u32>), ToolError> {
        {
            let table = self.lock();
            if table.processes.len() >= MAX_PROCESSES {
                return Err(ToolError::Validation(format!(
                    "already {MAX_PROCESSES} background processes; stop some with process_kill first"
                )));
            }
        }
        let mut cmd = shell_command(command);
        cmd.current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .map_err(|e| ToolError::RuntimeFailure(format!("failed to spawn command: {e}")))?;
        let pid = child.id();

        let output = Arc::new(Mutex::new(Output::default()));
        let exited = Arc::new(Mutex::new(None));
        let changed = Arc::new(tokio::sync::Notify::new());
        let readers: Vec<_> = [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        .map(|stream| tokio::spawn(collect(stream, output.clone(), changed.clone())))
        .collect();
        {
            let (exited, changed) = (exited.clone(), changed.clone());
            tokio::spawn(async move {
                let status = child.wait().await;
                // Let the readers take in the last output first.
                for reader in readers {
                    let _ = reader.await;
                }
                *exited.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(status.ok().and_then(|s| s.code()));
                changed.notify_waiters();
            });
        }

        let mut table = self.lock();
        table.next += 1;
        let handle = format!("bg-{}", table.next);
        table.processes.insert(
            handle.clone(),
            Process {
                command: command.to_string(),
                pid,
                output,
                exited,
                changed,
            },
        );
        Ok((handle, pid))
    }
}

impl ProcessTable {
    /// Stops every process tree still running. Called when the session
    /// closes, and again (harmlessly) on drop.
    pub fn kill_all(&self) {
        for process in self.lock().processes.values() {
            if process.running()
                && let Some(pid) = process.pid
            {
                kill_process_tree_blocking(pid);
            }
        }
    }
}

impl Drop for ProcessTable {
    /// The session is over: nothing may keep running on its behalf.
    fn drop(&mut self) {
        self.kill_all();
    }
}

/// Copies a stream into `output` until it closes.
async fn collect(
    mut stream: Box<dyn AsyncRead + Send + Unpin>,
    output: Arc<Mutex<Output>>,
    changed: Arc<tokio::sync::Notify>,
) {
    let mut buf = vec![0u8; 8 * 1024];
    let mut pending = Vec::new();
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                // Keep an incomplete UTF-8 sequence for the next read.
                let valid = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    Err(_) => pending.len(),
                };
                let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                pending.drain(..valid);
                output.lock().unwrap_or_else(|p| p.into_inner()).push(&text);
                changed.notify_waiters();
            }
        }
    }
    if !pending.is_empty() {
        let text = String::from_utf8_lossy(&pending).into_owned();
        output.lock().unwrap_or_else(|p| p.into_inner()).push(&text);
        changed.notify_waiters();
    }
}

#[derive(Deserialize, JsonSchema)]
struct OutputArgs {
    /// The handle `execute` returned (e.g. `bg-1`). Omit to list every
    /// background process with its status.
    #[serde(default)]
    handle: Option<String>,
    /// Seconds to wait for new output if there is none yet (0–30, default
    /// 0), e.g. while a server starts.
    #[serde(default)]
    wait_secs: Option<u64>,
}

/// Reads a background process's new output. Only reads what an approved
/// `execute` started, so it needs no approval of its own.
pub struct ProcessOutputTool {
    table: Arc<ProcessTable>,
}

impl ProcessOutputTool {
    pub fn new(table: Arc<ProcessTable>) -> Self {
        Self { table }
    }
}

#[async_trait]
impl ToolExecutor for ProcessOutputTool {
    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: OutputArgs = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid process_output arguments: {e}")))?;
        let Some(handle) = args.handle else {
            let table = self.table.lock();
            let list: Vec<Value> = table
                .processes
                .iter()
                .map(|(handle, p)| {
                    let mut entry = p.status();
                    entry["handle"] = json!(handle);
                    entry["command"] = json!(p.command);
                    entry
                })
                .collect();
            return Ok(ToolResult {
                id: invocation.id,
                output: json!({ "processes": list }),
                is_error: false,
                attachments: Vec::new(),
            });
        };

        let (output, changed) = {
            let table = self.table.lock();
            let process = table.processes.get(&handle).ok_or_else(|| {
                ToolError::Validation(format!("no background process {handle:?}"))
            })?;
            (process.output.clone(), process.changed.clone())
        };
        let wait = Duration::from_secs(args.wait_secs.unwrap_or(0).min(MAX_WAIT_SECS));
        let has_new = || {
            let out = output.lock().unwrap_or_else(|p| p.into_inner());
            out.end() > out.read
        };
        if !wait.is_zero() && !has_new() {
            let notified = changed.notified();
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep(wait) => {}
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }

        let (text, dropped) = output.lock().unwrap_or_else(|p| p.into_inner()).read_new();
        let mut result = {
            let table = self.table.lock();
            table
                .processes
                .get(&handle)
                .map(Process::status)
                .unwrap_or_else(|| json!({"status": "gone"}))
        };
        result["handle"] = json!(handle);
        result["output"] = json!(text);
        if dropped > 0 {
            result["dropped_bytes"] = json!(dropped);
        }
        Ok(ToolResult {
            id: invocation.id,
            output: result,
            is_error: false,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<OutputArgs>(
            "Read what a background process (started with `execute` and `background: true`) has printed since you last read it, and whether it's still running. Without a handle, lists all background processes.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    fn requires_approval(&self) -> bool {
        false
    }
}

#[derive(Deserialize, JsonSchema)]
struct KillArgs {
    /// The handle `execute` returned (e.g. `bg-1`).
    handle: String,
}

/// Stops a background process (its whole process tree) and forgets it.
pub struct ProcessKillTool {
    table: Arc<ProcessTable>,
}

impl ProcessKillTool {
    pub fn new(table: Arc<ProcessTable>) -> Self {
        Self { table }
    }
}

#[async_trait]
impl ToolExecutor for ProcessKillTool {
    fn subject(&self, arguments: &Value) -> Option<String> {
        arguments
            .get("handle")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: KillArgs = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid process_kill arguments: {e}")))?;
        let process = self
            .table
            .lock()
            .processes
            .remove(&args.handle)
            .ok_or_else(|| {
                ToolError::Validation(format!("no background process {:?}", args.handle))
            })?;
        let was_running = process.running();
        if was_running && let Some(pid) = process.pid {
            kill_process_tree(pid).await;
        }
        let (text, _) = process
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .read_new();
        Ok(ToolResult {
            id: invocation.id,
            output: json!({
                "handle": args.handle,
                "status": if was_running { "killed" } else { "already exited" },
                "last_output": text,
            }),
            is_error: false,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<KillArgs>(
            "Stop a background process started with `execute` (`background: true`), including anything it started, and forget its handle.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_capped_and_reads_return_only_new_text() {
        let mut out = Output::default();
        out.push("hello ");
        assert_eq!(out.read_new(), ("hello ".to_string(), 0));
        out.push("world");
        assert_eq!(out.read_new(), ("world".to_string(), 0));
        assert_eq!(out.read_new(), (String::new(), 0));
        // Overflow the cap without reading: the oldest unread bytes go.
        out.push(&"x".repeat(MAX_BUFFER_BYTES + 10));
        let (_, dropped) = out.read_new();
        assert_eq!(dropped, 10);
    }

    use crate::builtin::execute_tool::ExecuteTool;
    use arbe_core::{ToolCallId, TurnId};

    fn call(tool: &str, args: Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: tool.into(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    /// A command that prints a line, waits a bit, prints another, exits 3.
    fn two_lines_then_exit() -> &'static str {
        if cfg!(windows) {
            "echo first && ping -n 2 127.0.0.1 >nul && echo second && exit /b 3"
        } else {
            "echo first; sleep 1; echo second; exit 3"
        }
    }

    #[tokio::test]
    async fn a_background_command_is_read_incrementally_until_it_exits() {
        let dir = tempfile::tempdir().unwrap();
        let table = Arc::new(ProcessTable::new());
        let execute = ExecuteTool::with_processes(dir.path().to_path_buf(), table.clone());
        let output = ProcessOutputTool::new(table.clone());
        let ctx = ToolContext::for_testing();

        let started = execute
            .execute(
                call(
                    "execute",
                    json!({"command": two_lines_then_exit(), "background": true}),
                ),
                &ctx,
            )
            .await
            .unwrap();
        let handle = started.output["handle"].as_str().unwrap().to_string();
        assert_eq!(handle, "bg-1");

        // Collect everything until it has exited.
        let mut seen = String::new();
        let mut last = Value::Null;
        for _ in 0..40 {
            let r = output
                .execute(
                    call("process_output", json!({"handle": handle, "wait_secs": 1})),
                    &ctx,
                )
                .await
                .unwrap()
                .output;
            seen.push_str(r["output"].as_str().unwrap());
            let done = r["status"] == "exited";
            last = r;
            if done {
                break;
            }
        }
        assert!(
            seen.contains("first") && seen.contains("second"),
            "{seen:?}"
        );
        assert_eq!(last["exit_code"], 3);

        let list = output
            .execute(call("process_output", json!({})), &ctx)
            .await
            .unwrap()
            .output;
        assert_eq!(list["processes"][0]["handle"], "bg-1");
    }

    #[tokio::test]
    async fn a_background_command_can_be_killed() {
        let dir = tempfile::tempdir().unwrap();
        let table = Arc::new(ProcessTable::new());
        let execute = ExecuteTool::with_processes(dir.path().to_path_buf(), table.clone());
        let kill = ProcessKillTool::new(table.clone());
        let ctx = ToolContext::for_testing();
        let forever = if cfg!(windows) {
            "ping -n 600 127.0.0.1 >nul"
        } else {
            "sleep 600"
        };
        let started = execute
            .execute(
                call("execute", json!({"command": forever, "background": true})),
                &ctx,
            )
            .await
            .unwrap();
        let handle = started.output["handle"].as_str().unwrap().to_string();
        let killed = kill
            .execute(call("process_kill", json!({"handle": handle})), &ctx)
            .await
            .unwrap();
        assert_eq!(killed.output["status"], "killed");
        // Forgotten afterwards.
        assert!(
            kill.execute(call("process_kill", json!({"handle": handle})), &ctx)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn closing_the_session_stops_background_processes() {
        let dir = tempfile::tempdir().unwrap();
        let table = Arc::new(ProcessTable::new());
        let execute = ExecuteTool::with_processes(dir.path().to_path_buf(), table.clone());
        let output = ProcessOutputTool::new(table.clone());
        let ctx = ToolContext::for_testing();
        let forever = if cfg!(windows) {
            "ping -n 600 127.0.0.1 >nul"
        } else {
            "sleep 600"
        };
        execute
            .execute(
                call("execute", json!({"command": forever, "background": true})),
                &ctx,
            )
            .await
            .unwrap();
        execute.close();
        let mut status = Value::Null;
        for _ in 0..20 {
            status = output
                .execute(
                    call("process_output", json!({"handle": "bg-1", "wait_secs": 1})),
                    &ctx,
                )
                .await
                .unwrap()
                .output;
            if status["status"] == "exited" {
                break;
            }
        }
        assert_eq!(status["status"], "exited", "{status}");
    }

    #[test]
    fn capping_never_splits_a_character() {
        let mut out = Output::default();
        out.push(&"é".repeat(MAX_BUFFER_BYTES)); // 2 bytes each
        assert!(out.text.len() <= MAX_BUFFER_BYTES);
        assert!(out.text.chars().all(|c| c == 'é'));
    }
}
