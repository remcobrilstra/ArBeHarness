//! `--headless`: JSON-RPC 2.0 over stdio, one JSON message per line, so
//! editors, scripts and other UIs can drive the harness (v2 plan P6.2).
//!
//! Requests (client → harness): `initialize`, `session/new`,
//! `session/resume`, `session/list`, `session/set_title`, `session/close`,
//! `turn/send`, `turn/cancel`, `approval/decide`, `question/answer`,
//! `shutdown`. Every event
//! of an open session arrives as an `event` notification
//! (`{"session_id", "seq", "event"}`). Requests run concurrently: while
//! `turn/send` waits for its turn, the client can answer approvals or
//! cancel it. A turn's response is always written after every event the
//! turn produced. Nothing is approved on the client's behalf.
//!
//! The full protocol is documented in the user guide ("Headless mode").

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arbe_tui::arbe_runtime::arbe_core::{
    ApprovalDecision, EventEnvelope, HarnessError, RuntimeEvent, SessionId, StopReason, ToolCallId,
};
use arbe_tui::arbe_runtime::{Harness, Session};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// Bumped when the protocol changes incompatibly.
const PROTOCOL_VERSION: u32 = 1;

// JSON-RPC's own error codes...
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
// ...and the harness's.
const UNKNOWN_SESSION: i64 = -32001;
const BUSY: i64 = -32002;
const TURN_FAILED: i64 = -32003;
const TURN_CANCELLED: i64 = -32004;

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn unknown_session(id: SessionId) -> Self {
        Self::new(UNKNOWN_SESSION, format!("no open session {id}"))
    }

    /// For failures outside a turn (opening, listing, saving sessions).
    fn internal(err: impl std::fmt::Display) -> Self {
        Self::new(INTERNAL_ERROR, err.to_string())
    }
}

impl From<HarnessError> for RpcError {
    fn from(err: HarnessError) -> Self {
        let code = match err {
            HarnessError::Busy => BUSY,
            HarnessError::Cancelled => TURN_CANCELLED,
            _ => TURN_FAILED,
        };
        Self::new(code, err.to_string())
    }
}

type RpcResult = Result<Value, RpcError>;

fn response(id: &Value, result: RpcResult) -> String {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(err) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": err.code, "message": err.message},
        }),
    }
    .to_string()
}

fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// A `turn/send` result on its way to the session's forwarder, which
/// writes it after the turn's events.
struct TurnReply {
    request_id: Value,
    result: Result<String, HarnessError>,
}

struct OpenSession {
    session: Session,
    replies: mpsc::UnboundedSender<TurnReply>,
    forwarder: JoinHandle<()>,
}

struct Server {
    harness: Harness,
    out: mpsc::UnboundedSender<String>,
    sessions: Mutex<HashMap<SessionId, OpenSession>>,
}

/// Serves the protocol on stdin/stdout until `shutdown` or end of input.
pub async fn run(harness: Harness) -> i32 {
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    serve(harness, stdin, tokio::io::stdout()).await;
    0
}

pub async fn serve<R, W>(harness: Harness, input: R, output: W)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, lines) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(write_lines(lines, output));
    let server = Arc::new(Server {
        harness,
        out,
        sessions: Mutex::new(HashMap::new()),
    });

    let mut input = input.lines();
    let mut requests = Vec::new();
    loop {
        let line = match input.next_line().await {
            Ok(Some(line)) => line,
            // End of input, or unreadable input: shut down either way.
            Ok(None) | Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(err) => {
                server.send(response(
                    &Value::Null,
                    Err(RpcError::new(PARSE_ERROR, format!("invalid JSON: {err}"))),
                ));
                continue;
            }
        };
        let Some((id, method, params)) = split_request(&server, message) else {
            continue;
        };
        if method == "shutdown" {
            if let Some(id) = &id {
                server.send(response(id, Ok(json!({}))));
            }
            break;
        }
        let server = server.clone();
        requests.push(tokio::spawn(async move {
            server.handle(id, &method, params).await;
        }));
        requests.retain(|r: &JoinHandle<()>| !r.is_finished());
    }

    server.close_all().await;
    for request in requests {
        request.abort();
    }
    drop(server);
    // Every sender is gone once the server and its tasks are: the writer
    // flushes what's queued and ends.
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

/// Checks the JSON-RPC envelope. Returns `(id, method, params)`, with no
/// id for a notification; answers invalid requests itself.
fn split_request(server: &Server, message: Value) -> Option<(Option<Value>, String, Value)> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    match (message.get("jsonrpc").and_then(Value::as_str), method) {
        (Some("2.0"), Some(method)) => Some((
            id,
            method.to_string(),
            message.get("params").cloned().unwrap_or(Value::Null),
        )),
        _ => {
            server.send(response(
                &id.unwrap_or(Value::Null),
                Err(RpcError::new(
                    INVALID_REQUEST,
                    "expected a JSON-RPC 2.0 request with a method",
                )),
            ));
            None
        }
    }
}

async fn write_lines<W: AsyncWrite + Unpin>(
    mut lines: mpsc::UnboundedReceiver<String>,
    mut output: W,
) {
    while let Some(line) = lines.recv().await {
        let written = async {
            output.write_all(line.as_bytes()).await?;
            output.write_all(b"\n").await?;
            output.flush().await
        };
        if written.await.is_err() {
            // The client has gone; nothing more can be delivered.
            break;
        }
    }
}

fn params<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    // Methods without parameters accept an absent/`null` params.
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError::new(INVALID_PARAMS, e.to_string()))
}

#[derive(Deserialize)]
struct NewParams {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ResumeParams {
    session_id: SessionId,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct SessionParams {
    session_id: SessionId,
}

#[derive(Deserialize)]
struct TitleParams {
    session_id: SessionId,
    title: String,
}

#[derive(Deserialize)]
struct SendParams {
    session_id: SessionId,
    message: String,
}

#[derive(Deserialize)]
struct AnswerParams {
    session_id: SessionId,
    question_id: ToolCallId,
    answer: String,
}

#[derive(Deserialize)]
struct DecideParams {
    session_id: SessionId,
    tool_call_id: ToolCallId,
    decision: ApprovalDecision,
}

impl Server {
    fn send(&self, line: String) {
        let _ = self.out.send(line);
    }

    async fn handle(self: Arc<Self>, id: Option<Value>, method: &str, raw: Value) {
        let result = match method {
            "initialize" => Ok(json!({
                "server": "arbeharness",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
            })),
            "session/new" => params::<NewParams>(raw).and_then(|p| self.open(None, p.name)),
            "session/resume" => {
                params::<ResumeParams>(raw).and_then(|p| self.open(Some(p.session_id), p.name))
            }
            "session/list" => self
                .harness
                .sessions()
                .map(|sessions| json!({"sessions": sessions}))
                .map_err(RpcError::internal),
            "session/set_title" => params::<TitleParams>(raw).and_then(|p| {
                self.with_session(p.session_id, |s| {
                    s.set_title(p.title).map_err(RpcError::internal)
                })?;
                Ok(json!({}))
            }),
            "session/close" => params::<SessionParams>(raw).and_then(|p| self.close(p.session_id)),
            "turn/send" => match params::<SendParams>(raw) {
                // Answered by the session's forwarder, after the turn's events.
                Ok(p) => match self.send_turn(id.clone(), p) {
                    Ok(()) => return,
                    Err(err) => Err(err),
                },
                Err(err) => Err(err),
            },
            "turn/cancel" => params::<SessionParams>(raw).and_then(|p| {
                let cancelled = self.with_session(p.session_id, |s| Ok(s.cancel()))?;
                Ok(json!({"cancelled": cancelled}))
            }),
            "approval/decide" => params::<DecideParams>(raw).and_then(|p| {
                let accepted =
                    self.with_session(p.session_id, |s| Ok(s.decide(p.tool_call_id, p.decision)))?;
                Ok(json!({"accepted": accepted}))
            }),
            "question/answer" => params::<AnswerParams>(raw).and_then(|p| {
                let accepted =
                    self.with_session(p.session_id, |s| Ok(s.answer(p.question_id, p.answer)))?;
                Ok(json!({"accepted": accepted}))
            }),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )),
        };
        // Notifications get no response, not even an error.
        if let Some(id) = id {
            self.send(response(&id, result));
        }
    }

    fn with_session<T>(
        &self,
        id: SessionId,
        f: impl FnOnce(&Session) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .map(|open| open.session.clone())
            .ok_or_else(|| RpcError::unknown_session(id))?;
        f(&session)
    }

    /// Opens a new session, or resumes `id`, and starts forwarding its
    /// events.
    fn open(&self, id: Option<SessionId>, name: Option<String>) -> RpcResult {
        if let Some(id) = id
            && self
                .sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains_key(&id)
        {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("session {id} is already open"),
            ));
        }
        let session = match id {
            Some(id) => {
                if self.harness.store().load_meta(id).is_err() {
                    return Err(RpcError::new(
                        UNKNOWN_SESSION,
                        format!("no saved session {id}"),
                    ));
                }
                self.harness.resume_session(id)
            }
            None => self.harness.new_session(),
        }
        .map_err(RpcError::internal)?;
        if let Some(name) = name {
            session.set_title(name).map_err(RpcError::internal)?;
        }
        let session_id = session.id();
        let (replies, replies_rx) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(
            session_id,
            session.subscribe(),
            replies_rx,
            self.out.clone(),
        ));
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                session_id,
                OpenSession {
                    session,
                    replies,
                    forwarder,
                },
            );
        self.session_info(session_id)
    }

    fn session_info(&self, id: SessionId) -> RpcResult {
        let meta = self
            .harness
            .store()
            .load_meta(id)
            .map_err(RpcError::internal)?;
        Ok(json!({"session_id": id, "session": meta}))
    }

    fn send_turn(&self, request_id: Option<Value>, p: SendParams) -> Result<(), RpcError> {
        let (session, replies) = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let open = sessions
                .get(&p.session_id)
                .ok_or_else(|| RpcError::unknown_session(p.session_id))?;
            (open.session.clone(), open.replies.clone())
        };
        tokio::spawn(async move {
            let result = session.agent().submit_message(p.message).await;
            if let Some(request_id) = request_id {
                let _ = replies.send(TurnReply { request_id, result });
            }
        });
        Ok(())
    }

    fn close(&self, id: SessionId) -> RpcResult {
        let open = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id)
            .ok_or_else(|| RpcError::unknown_session(id))?;
        open.session.cancel();
        open.session.close().map_err(RpcError::internal)?;
        // Dropping `open` ends its forwarder once it has flushed.
        Ok(json!({}))
    }

    /// Stops every running turn and closes every session.
    async fn close_all(&self) {
        let open: Vec<OpenSession> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, open)| open)
            .collect();
        for session in &open {
            session.session.cancel();
        }
        for OpenSession {
            session,
            replies,
            forwarder,
        } in open
        {
            // Give a cancelled turn a moment to save what it has.
            for _ in 0..40 {
                if !session.agent().is_busy() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let _ = session.close();
            drop(replies);
            let _ = tokio::time::timeout(Duration::from_secs(1), forwarder).await;
        }
    }
}

/// Writes a session's events as notifications and its turns' responses,
/// each response after every event its turn produced (they were all
/// published before the turn returned, so they're already queued).
async fn forward(
    session_id: SessionId,
    mut events: broadcast::Receiver<EventEnvelope>,
    mut replies: mpsc::UnboundedReceiver<TurnReply>,
    out: mpsc::UnboundedSender<String>,
) {
    // The stop reason of the latest completed turn, for its response.
    let mut last_stop: Option<StopReason> = None;
    let emit = |envelope: EventEnvelope, last_stop: &mut Option<StopReason>| {
        if let RuntimeEvent::TurnCompleted { stop_reason, .. } = &envelope.event {
            *last_stop = Some(stop_reason.clone());
        }
        let _ = out.send(notification(
            "event",
            json!({"session_id": session_id, "seq": envelope.seq, "event": envelope.event}),
        ));
    };
    loop {
        tokio::select! {
            biased;
            reply = replies.recv() => {
                let Some(reply) = reply else { break };
                loop {
                    match events.try_recv() {
                        Ok(envelope) => emit(envelope, &mut last_stop),
                        Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                let result = reply
                    .result
                    .map(|answer| json!({"answer": answer, "stop_reason": last_stop.take()}))
                    .map_err(RpcError::from);
                let _ = out.send(response(&reply.request_id, result));
            }
            received = events.recv() => match received {
                Ok(envelope) => emit(envelope, &mut last_stop),
                // A gap shows up as a jump in `seq`.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_tui::arbe_runtime::arbe_core::{ProviderError, Role, Usage};
    use arbe_tui::arbe_runtime::arbe_providers::{
        CancellationToken, ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent,
        ProviderStream,
    };
    use async_trait::async_trait;
    use tokio::io::BufReader;

    /// Asks to read `a.txt`, then says whether the read returned its text.
    struct ReadThenAnswer;

    #[async_trait]
    impl ModelProvider for ReadThenAnswer {
        fn id(&self) -> &str {
            "scripted"
        }
        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities {
                streaming: true,
                tool_calls: true,
                vision: false,
                thinking: false,
                prompt_caching: false,
                max_context_tokens: 32_000,
            }
        }
        async fn stream(
            &self,
            req: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            let last = req.messages.last().unwrap();
            let events = if last.role == Role::Tool {
                vec![
                    ProviderEvent::TextDelta(
                        if serde_json::to_string(&last.content)
                            .unwrap()
                            .contains("hello from a.txt")
                        {
                            "the file says hello".into()
                        } else {
                            "the read failed".into()
                        },
                    ),
                    ProviderEvent::Usage(Usage::default()),
                    ProviderEvent::Stop(StopReason::EndTurn),
                ]
            } else {
                vec![
                    ProviderEvent::ToolUseStart {
                        id: "c1".into(),
                        name: "read_file".into(),
                    },
                    ProviderEvent::ToolUseInputDelta {
                        id: "c1".into(),
                        partial_json: r#"{"path":"a.txt"}"#.into(),
                    },
                    ProviderEvent::ToolUseEnd { id: "c1".into() },
                    ProviderEvent::Stop(StopReason::ToolUse),
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    struct Client {
        input: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        output: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        seen: Vec<Value>,
    }

    impl Client {
        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.input.write_all(line.as_bytes()).await.unwrap();
        }

        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), self.output.next_line())
                .await
                .expect("no message within 5 s")
                .unwrap()
                .expect("server closed the stream");
            let message: Value = serde_json::from_str(&line).unwrap();
            self.seen.push(message.clone());
            message
        }

        /// Reads until a message matches.
        async fn until(&mut self, pred: impl Fn(&Value) -> bool) -> Value {
            loop {
                let message = self.next().await;
                if pred(&message) {
                    return message;
                }
            }
        }

        async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .await;
            self.until(|m| m["id"] == id).await
        }
    }

    fn start() -> (Client, JoinHandle<()>, tempfile::TempDir, tempfile::TempDir) {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("a.txt"), "hello from a.txt").unwrap();
        let harness = Harness::builder()
            .ignore_env()
            .home(home.path())
            .project_dir(project.path())
            .register_provider("scripted", |_| {
                Ok(Box::new(ReadThenAnswer) as Box<dyn ModelProvider>)
            })
            .provider("scripted")
            .model("any")
            .build()
            .unwrap();
        let (client_end, server_end) = tokio::io::duplex(1 << 16);
        let (server_read, server_write) = tokio::io::split(server_end);
        let server = tokio::spawn(serve(harness, BufReader::new(server_read), server_write));
        let (client_read, client_write) = tokio::io::split(client_end);
        let client = Client {
            input: client_write,
            output: BufReader::new(client_read).lines(),
            seen: Vec::new(),
        };
        (client, server, home, project)
    }

    #[tokio::test]
    async fn a_turn_asks_for_approval_and_answers_after_its_events() {
        let (mut client, server, _home, _project) = start();
        let init = client.call(1, "initialize", Value::Null).await;
        assert_eq!(init["result"]["protocol_version"], PROTOCOL_VERSION);

        let opened = client.call(2, "session/new", json!({"name": "rpc"})).await;
        let session_id = opened["result"]["session_id"].clone();
        assert_eq!(opened["result"]["session"]["title"], "rpc");

        client
            .send(json!({"jsonrpc": "2.0", "id": 3, "method": "turn/send",
                "params": {"session_id": session_id, "message": "read a.txt"}}))
            .await;
        let approval = client
            .until(|m| m["params"]["event"]["type"] == "tool_approval_requested")
            .await;
        let decided = client
            .call(
                4,
                "approval/decide",
                json!({"session_id": session_id,
                    "tool_call_id": approval["params"]["event"]["tool_call_id"],
                    "decision": "approved_once"}),
            )
            .await;
        assert_eq!(decided["result"]["accepted"], true);

        let answer = client.until(|m| m["id"] == 3).await;
        assert_eq!(answer["result"]["answer"], "the file says hello");
        assert_eq!(answer["result"]["stop_reason"]["kind"], "end_turn");
        // Everything the turn produced came before its response.
        let completed = client
            .seen
            .iter()
            .position(|m| m["params"]["event"]["type"] == "turn_completed")
            .expect("turn_completed was sent");
        let answered = client.seen.iter().position(|m| m["id"] == 3).unwrap();
        assert!(completed < answered);

        let shutdown = client.call(5, "shutdown", Value::Null).await;
        assert_eq!(shutdown["result"], json!({}));
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("server did not stop")
            .unwrap();
    }

    #[tokio::test]
    async fn protocol_errors_have_their_codes() {
        let (mut client, _server, _home, _project) = start();
        let unknown = client.call(1, "nope", Value::Null).await;
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);
        let bad_params = client.call(2, "turn/send", json!({"message": "x"})).await;
        assert_eq!(bad_params["error"]["code"], INVALID_PARAMS);
        let no_session = client
            .call(3, "turn/cancel", json!({"session_id": SessionId::new()}))
            .await;
        assert_eq!(no_session["error"]["code"], UNKNOWN_SESSION);
        let no_saved = client
            .call(4, "session/resume", json!({"session_id": SessionId::new()}))
            .await;
        assert_eq!(no_saved["error"]["code"], UNKNOWN_SESSION);

        client.input.write_all(b"{not json\n").await.unwrap();
        assert_eq!(client.next().await["error"]["code"], PARSE_ERROR);
        client.send(json!({"id": 5, "method": "initialize"})).await;
        assert_eq!(client.next().await["error"]["code"], INVALID_REQUEST);
        // A notification gets no response, even for an unknown method...
        client
            .send(json!({"jsonrpc": "2.0", "method": "nope"}))
            .await;
        // ...so the next message is the response to this request.
        let init = client.call(6, "initialize", Value::Null).await;
        assert_eq!(client.seen.len(), 7);
        assert!(init["result"].is_object());
    }

    #[tokio::test]
    async fn end_of_input_closes_every_session() {
        let (mut client, server, home, _project) = start();
        let opened = client.call(1, "session/new", Value::Null).await;
        let id = opened["result"]["session_id"].as_str().unwrap().to_string();
        drop(client);
        tokio::time::timeout(Duration::from_secs(10), server)
            .await
            .expect("server did not stop on end of input")
            .unwrap();
        let meta: Value = serde_json::from_slice(
            &std::fs::read(home.path().join("sessions").join(&id).join("meta.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(meta["status"], "closed");
    }
}
