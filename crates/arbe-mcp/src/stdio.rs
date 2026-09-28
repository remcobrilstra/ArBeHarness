//! stdio transport: the harness starts the server process and exchanges
//! newline-delimited JSON-RPC over its stdin/stdout.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::client::{Router, Transport};
use crate::protocol::{McpError, classify};

pub(crate) struct StdioTransport {
    stdin: Arc<Mutex<ChildStdin>>,
    /// Kept so the process is killed when the transport is dropped
    /// (`kill_on_drop`).
    _child: Child,
    reader: JoinHandle<()>,
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// On Windows, `npx`, `uvx` and friends are `.cmd` scripts that
/// `CreateProcess` won't find by bare name; running through `cmd /C`
/// resolves them the way a terminal would.
fn command_for(program: &str, args: &[String], via_shell: bool) -> Command {
    if via_shell {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(program).args(args);
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    }
}

impl StdioTransport {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        name: &str,
        program: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&Path>,
        log_dir: Option<&Path>,
        router: Arc<Router>,
    ) -> Result<Self, McpError> {
        let stderr = || -> Stdio {
            let Some(dir) = log_dir else {
                return Stdio::null();
            };
            let _ = std::fs::create_dir_all(dir);
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(format!("{name}.log")))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null())
        };
        let start = |via_shell: bool| {
            let mut command = command_for(program, args, via_shell);
            command
                .envs(env)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(stderr())
                .kill_on_drop(true);
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            command.spawn()
        };
        let mut child = match start(false) {
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::NotFound => start(true),
            other => other,
        }
        .map_err(|e| McpError::Transport(format!("failed to start {program:?}: {e}")))?;

        let stdin =
            Arc::new(Mutex::new(child.stdin.take().ok_or_else(|| {
                McpError::Transport("server process has no stdin".into())
            })?));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("server process has no stdout".into()))?;

        let reply_to = stdin.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        match classify(line) {
                            Ok(incoming) => {
                                if let Some(reply) = router.dispatch(incoming) {
                                    let _ = write_line(&reply_to, &reply).await;
                                }
                            }
                            // Servers sometimes print non-protocol noise to
                            // stdout; skip it rather than dropping the link.
                            Err(err) => {
                                tracing::debug!(%err, "ignoring non-JSON-RPC line from mcp server")
                            }
                        }
                    }
                    Ok(None) => {
                        router.close("the server process exited");
                        return;
                    }
                    Err(err) => {
                        router.close(&format!("reading from the server failed: {err}"));
                        return;
                    }
                }
            }
        });

        Ok(Self {
            stdin,
            _child: child,
            reader,
        })
    }
}

async fn write_line(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<(), McpError> {
    let mut line = serde_json::to_string(message).map_err(|e| McpError::Parse(e.to_string()))?;
    line.push('\n');
    let mut stdin = stdin.lock().await;
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| McpError::Transport(format!("writing to the server failed: {e}")))?;
    stdin
        .flush()
        .await
        .map_err(|e| McpError::Transport(format!("writing to the server failed: {e}")))
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, message: &Value) -> Result<(), McpError> {
        write_line(&self.stdin, message).await
    }
}
