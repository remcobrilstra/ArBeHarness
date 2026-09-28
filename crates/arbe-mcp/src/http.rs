//! Streamable HTTP transport: each message is POSTed to the server's
//! endpoint; the reply is either a JSON body or a server-sent-event stream
//! carrying the response (and possibly notifications/requests before it).
//!
//! Not supported: the optional standalone GET stream for server-initiated
//! messages, so over HTTP the client only hears from the server while a
//! request is open (a `tools/list_changed` sent at another time is missed).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::Value;

use crate::client::{Router, Transport};
use crate::protocol::{Incoming, McpError, classify_value};

const SESSION_HEADER: &str = "Mcp-Session-Id";
const VERSION_HEADER: &str = "MCP-Protocol-Version";

struct Inner {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    bearer_token: Option<String>,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    router: Arc<Router>,
}

pub(crate) struct HttpTransport {
    inner: Arc<Inner>,
}

impl HttpTransport {
    pub(crate) fn new(
        url: &str,
        headers: &BTreeMap<String, String>,
        bearer_token: Option<String>,
        router: Arc<Router>,
    ) -> Result<Self, McpError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                url: url.to_string(),
                headers: headers.clone(),
                bearer_token,
                session_id: Mutex::new(None),
                protocol_version: Mutex::new(None),
                router,
            }),
        })
    }
}

impl Inner {
    async fn post(self: &Arc<Self>, message: &Value) -> Result<(), McpError> {
        let mut request = self
            .client
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .json(message);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(token) = &self.bearer_token {
            request = request.bearer_auth(token);
        }
        if let Some(session) = self
            .session_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(version) = self
            .protocol_version
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            request = request.header(VERSION_HEADER, version);
        }

        let response = request
            .send()
            .await
            .map_err(|e| McpError::Transport(format!("request to {} failed: {e}", self.url)))?;
        let status = response.status();
        if let Some(session) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().unwrap_or_else(|p| p.into_inner()) = Some(session.to_string());
        }
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!("HTTP {status}: {body}")));
        }

        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        if is_sse {
            let mut decoder = SseDecoder::default();
            let mut body = response.bytes_stream();
            while let Some(chunk) = body.next().await {
                let chunk = chunk.map_err(|e| McpError::Transport(e.to_string()))?;
                for data in decoder.push(&String::from_utf8_lossy(&chunk)) {
                    self.receive(&data);
                }
            }
        } else {
            let text = response
                .text()
                .await
                .map_err(|e| McpError::Transport(e.to_string()))?;
            if !text.trim().is_empty() {
                self.receive(&text);
            }
        }
        Ok(())
    }

    /// Routes one received JSON-RPC payload (a message or a batch).
    fn receive(self: &Arc<Self>, text: &str) {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            tracing::debug!("ignoring non-JSON payload from mcp server");
            return;
        };
        let messages = match value {
            Value::Array(batch) => batch,
            single => vec![single],
        };
        for message in messages {
            // The negotiated version goes on every later request.
            if let Some(version) = message
                .get("result")
                .and_then(|r| r.get("protocolVersion"))
                .and_then(Value::as_str)
            {
                *self
                    .protocol_version
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(version.to_string());
            }
            let Ok(incoming) = classify_value(message) else {
                continue;
            };
            if let Some(reply) = self.router.dispatch(incoming) {
                let this = self.clone();
                tokio::spawn(async move {
                    let _ = this.post(&reply).await;
                });
            }
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, message: &Value) -> Result<(), McpError> {
        let request_id = message
            .get("method")
            .and(message.get("id"))
            .and_then(Value::as_u64);
        match request_id {
            // A request: run the exchange in the background so the
            // client's timeout/cancellation covers it; a failure is
            // delivered as that request's response.
            Some(id) => {
                let inner = self.inner.clone();
                let message = message.clone();
                tokio::spawn(async move {
                    if let Err(err) = inner.post(&message).await {
                        inner.router.dispatch(Incoming::Response {
                            id,
                            outcome: Err((0, err.to_string())),
                        });
                    }
                });
                Ok(())
            }
            None => self.inner.post(message).await,
        }
    }
}

/// Minimal server-sent-events decoder: returns each event's `data`
/// (multi-line data joined with `\n`), buffering partial lines across
/// chunks.
#[derive(Default)]
struct SseDecoder {
    buffer: String,
    data: Vec<String>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..=pos);
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_are_split_on_blank_lines_across_chunks() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push("event: message\ndata: {\"a\"").is_empty());
        assert_eq!(decoder.push(":1}\r\n\r\n"), vec![r#"{"a":1}"#.to_string()]);
        assert_eq!(
            decoder.push("data: line1\ndata: line2\n\n: comment\n\n"),
            vec!["line1\nline2".to_string()]
        );
    }
}
