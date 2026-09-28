//! An MCP client over any transport. Requests are matched to responses by
//! id, so several can be in flight at once (e.g. parallel tool calls);
//! server notifications and server-to-client requests are handled as they
//! arrive.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::config::{McpServerConfig, TransportConfig};
use crate::protocol::{
    Incoming, METHOD_NOT_FOUND, McpError, PROTOCOL_VERSION, build_error, build_notification,
    build_request, build_result,
};

/// Moves JSON-RPC messages to and from a server. Incoming messages are
/// handed to the [`Router`] the transport was created with.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn send(&self, message: &Value) -> Result<(), McpError>;
}

type Pending = HashMap<u64, oneshot::Sender<Result<Value, McpError>>>;

/// Delivers responses to the requests waiting for them and records what
/// notifications said.
#[derive(Default)]
pub(crate) struct Router {
    pending: Mutex<Pending>,
    tools_changed: AtomicBool,
    closed: Mutex<Option<String>>,
}

impl Router {
    fn register(&self, id: u64) -> oneshot::Receiver<Result<Value, McpError>> {
        let (tx, rx) = oneshot::channel();
        self.lock_pending().insert(id, tx);
        rx
    }

    fn forget(&self, id: u64) {
        self.lock_pending().remove(&id);
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Handles one incoming message. Returns a reply to send back if the
    /// server asked us something.
    pub(crate) fn dispatch(&self, incoming: Incoming) -> Option<Value> {
        match incoming {
            Incoming::Response { id, outcome } => {
                if let Some(tx) = self.lock_pending().remove(&id) {
                    let _ =
                        tx.send(outcome.map_err(|(code, message)| McpError::Rpc { code, message }));
                }
                None
            }
            Incoming::Notification { method, params } => {
                match method.as_str() {
                    "notifications/tools/list_changed" => {
                        self.tools_changed.store(true, Ordering::SeqCst);
                    }
                    "notifications/message" => tracing::debug!(%params, "mcp server log"),
                    _ => {}
                }
                None
            }
            // We declare no client capabilities, so the only request we
            // serve is the liveness check.
            Incoming::Request { id, method, .. } => Some(if method == "ping" {
                build_result(id, json!({}))
            } else {
                build_error(
                    id,
                    METHOD_NOT_FOUND,
                    &format!("method not supported: {method}"),
                )
            }),
        }
    }

    /// The connection is gone: fail every waiting request.
    pub(crate) fn close(&self, reason: &str) {
        *self.closed.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason.to_string());
        for (_, tx) in self.lock_pending().drain() {
            let _ = tx.send(Err(McpError::Transport(reason.to_string())));
        }
    }

    fn closed_reason(&self) -> Option<String> {
        self.closed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

/// What a server says about one of its tools.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub input_schema: Value,
    /// `annotations.readOnlyHint`: the tool doesn't change anything.
    pub read_only: bool,
    /// `annotations.destructiveHint`: the tool may destroy data.
    pub destructive: bool,
}

/// A tool call's outcome, reduced to what the model reads.
#[derive(Debug, Clone, PartialEq)]
pub struct CallOutcome {
    pub text: String,
    pub is_error: bool,
}

pub struct McpClient {
    name: String,
    transport: Box<dyn Transport>,
    router: Arc<Router>,
    next_id: AtomicU64,
    timeout: Duration,
}

impl McpClient {
    /// Starts/opens the transport and performs the `initialize` handshake.
    /// A stdio server's stderr is appended to `<log_dir>/<name>.log` when
    /// `log_dir` is given.
    pub async fn connect(
        config: &McpServerConfig,
        log_dir: Option<&Path>,
    ) -> Result<Self, McpError> {
        let router = Arc::new(Router::default());
        let transport: Box<dyn Transport> = match &config.transport {
            TransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => Box::new(crate::stdio::StdioTransport::spawn(
                &config.name,
                command,
                args,
                env,
                cwd.as_deref(),
                log_dir,
                router.clone(),
            )?),
            TransportConfig::Http {
                url,
                headers,
                bearer_token,
            } => Box::new(crate::http::HttpTransport::new(
                url,
                headers,
                bearer_token.clone(),
                router.clone(),
            )?),
        };
        let client = Self {
            name: config.name.clone(),
            transport,
            router,
            next_id: AtomicU64::new(1),
            timeout: config.timeout,
        };
        client.initialize().await?;
        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the connection has ended (the process exited, the stream
    /// closed). A closed client fails every request; reconnect instead.
    pub fn is_closed(&self) -> bool {
        self.router.closed_reason().is_some()
    }

    /// Whether the server said its tool list changed since the last call.
    pub fn take_tools_changed(&self) -> bool {
        self.router.tools_changed.swap(false, Ordering::SeqCst)
    }

    /// Sends a request and waits for its response, the timeout, or
    /// `cancel`. On timeout or cancellation the server is told
    /// (`notifications/cancelled`) so it can stop the work.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, McpError> {
        if let Some(reason) = self.router.closed_reason() {
            return Err(McpError::Transport(reason));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let rx = self.router.register(id);
        if let Err(err) = self
            .transport
            .send(&build_request(id, method, params))
            .await
        {
            self.router.forget(id);
            return Err(err);
        }
        let never = CancellationToken::new();
        let cancel = cancel.unwrap_or(&never);
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(McpError::Cancelled),
            _ = tokio::time::sleep(self.timeout) => Err(McpError::Timeout(self.timeout)),
            response = rx => return response.unwrap_or_else(|_| {
                Err(McpError::Transport("connection closed".to_string()))
            }),
        };
        self.router.forget(id);
        let reason = match &outcome {
            Err(McpError::Timeout(_)) => "timed out",
            _ => "cancelled by the user",
        };
        let _ = self
            .notify(
                "notifications/cancelled",
                json!({ "requestId": id, "reason": reason }),
            )
            .await;
        outcome
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.transport
            .send(&build_notification(method, params))
            .await
    }

    async fn initialize(&self) -> Result<(), McpError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "arbeharness", "version": env!("CARGO_PKG_VERSION") },
            }),
            None,
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    /// Every tool the server offers (following pagination).
    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, None).await?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Parse("tools/list result has no tools array".into()))?;
            for tool in page {
                tools.push(parse_tool(tool)?);
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<CallOutcome, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                cancel,
            )
            .await?;
        Ok(call_outcome(&result))
    }
}

fn parse_tool(tool: &Value) -> Result<McpToolInfo, McpError> {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::Parse("tool entry missing name".into()))?
        .to_string();
    let annotation = |key: &str| {
        tool.get("annotations")
            .and_then(|a| a.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Ok(McpToolInfo {
        name,
        description: tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_schema: tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" })),
        read_only: annotation("readOnlyHint"),
        destructive: annotation("destructiveHint"),
    })
}

/// Flattens a `tools/call` result into text: text content as-is,
/// resources by their text or URI, images as a placeholder (tool results
/// are text-only for now), and `structuredContent` if there's nothing else.
pub(crate) fn call_outcome(result: &Value) -> CallOutcome {
    let mut parts = Vec::new();
    for item in result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(item["text"].as_str().unwrap_or_default().to_string()),
            Some("image") => parts.push(format!(
                "[image: {}]",
                item["mimeType"].as_str().unwrap_or("unknown type")
            )),
            Some("resource") => parts.push(
                item["resource"]["text"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                            "[resource: {}]",
                            item["resource"]["uri"].as_str().unwrap_or("?")
                        )
                    }),
            ),
            Some("resource_link") => parts.push(format!(
                "[resource: {}]",
                item["uri"].as_str().unwrap_or("?")
            )),
            _ => {}
        }
    }
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        parts.push(structured.to_string());
    }
    CallOutcome {
        text: parts.join("\n"),
        is_error: result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_router_delivers_responses_by_id_and_fails_the_rest_on_close() {
        let router = Router::default();
        let mut first = router.register(1);
        let mut second = router.register(2);
        router.dispatch(Incoming::Response {
            id: 2,
            outcome: Ok(json!("two")),
        });
        assert_eq!(second.try_recv().unwrap().unwrap(), json!("two"));
        assert!(first.try_recv().is_err(), "1 is still waiting");
        router.close("server exited");
        assert!(matches!(
            first.try_recv().unwrap(),
            Err(McpError::Transport(_))
        ));
        assert_eq!(router.closed_reason().as_deref(), Some("server exited"));
    }

    #[test]
    fn the_router_answers_pings_and_refuses_other_requests() {
        let router = Router::default();
        let pong = router
            .dispatch(Incoming::Request {
                id: json!(9),
                method: "ping".into(),
                params: Value::Null,
            })
            .unwrap();
        assert_eq!(pong["result"], json!({}));
        let refused = router
            .dispatch(Incoming::Request {
                id: json!(10),
                method: "sampling/createMessage".into(),
                params: Value::Null,
            })
            .unwrap();
        assert_eq!(refused["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn list_changed_notifications_are_remembered_until_taken() {
        let router = Router::default();
        router.dispatch(Incoming::Notification {
            method: "notifications/tools/list_changed".into(),
            params: Value::Null,
        });
        assert!(router.tools_changed.swap(false, Ordering::SeqCst));
        assert!(!router.tools_changed.load(Ordering::SeqCst));
    }

    #[test]
    fn tool_entries_keep_their_schema_and_hints() {
        let tool = parse_tool(&json!({
            "name": "search",
            "description": "Search issues",
            "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
            "annotations": {"readOnlyHint": true}
        }))
        .unwrap();
        assert_eq!(tool.name, "search");
        assert_eq!(tool.input_schema["properties"]["q"]["type"], "string");
        assert!(tool.read_only);
        assert!(!tool.destructive);
        // A tool without a schema accepts any object.
        assert_eq!(
            parse_tool(&json!({"name": "x"})).unwrap().input_schema,
            json!({"type": "object"})
        );
    }

    #[test]
    fn call_results_flatten_to_text() {
        let outcome = call_outcome(&json!({
            "content": [
                {"type": "text", "text": "line one"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///a", "text": "file body"}},
                {"type": "resource_link", "uri": "file:///b"}
            ],
            "isError": true
        }));
        assert_eq!(
            outcome.text,
            "line one\n[image: image/png]\nfile body\n[resource: file:///b]"
        );
        assert!(outcome.is_error);
        let structured = call_outcome(&json!({"structuredContent": {"n": 1}}));
        assert_eq!(structured.text, r#"{"n":1}"#);
        assert!(!structured.is_error);
    }
}
