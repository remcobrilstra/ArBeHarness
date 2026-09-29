//! Process- and network-level tests for the MCP client, against the
//! fixture server (`tests/fixture/mcp_fixture_server.rs`) and a minimal
//! in-test HTTP server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arbe_core::{RiskLevel, ToolCallId, ToolError, ToolInvocation, TurnId};
use arbe_mcp::{
    McpClient, McpError, McpManager, McpServer, McpServerConfig, ServerStatus, ToolSink,
    TransportConfig,
};
use arbe_tools::{CancellationToken, ToolContext, ToolExecutor};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixture(name: &str, timeout: Duration) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: TransportConfig::Stdio {
            command: env!("CARGO_BIN_EXE_mcp-fixture-server").to_string(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout,
    }
}

async fn connect() -> McpClient {
    McpClient::connect(&fixture("fixture", Duration::from_secs(10)), None)
        .await
        .expect("fixture server should start")
}

#[tokio::test]
async fn lists_tools_and_calls_them_over_stdio() {
    let client = connect().await;
    // Startup noise on stdout and a notification before the response are
    // both skipped.
    let tools = client.list_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "add", "slow", "fail", "crash", "toggle"]);
    assert!(tools[0].read_only);
    assert!(tools[4].destructive);
    assert_eq!(tools[0].input_schema["required"], json!(["text"]));

    let echo = client
        .call_tool("echo", json!({"text": "hi"}), None)
        .await
        .unwrap();
    assert_eq!(echo.text, "hi");
    assert!(!echo.is_error);
    assert_eq!(
        client
            .call_tool("add", json!({"a": 2, "b": 3}), None)
            .await
            .unwrap()
            .text,
        "5"
    );

    let failed = client.call_tool("fail", json!({}), None).await.unwrap();
    assert!(failed.is_error);
    assert_eq!(failed.text, "it failed");

    // A JSON-RPC error from the server is an Rpc error, not a transport one.
    assert!(matches!(
        client.call_tool("nope", json!({}), None).await,
        Err(McpError::Rpc { code: -32602, .. })
    ));
}

#[tokio::test]
async fn concurrent_calls_are_matched_to_their_own_responses() {
    let client = Arc::new(connect().await);
    let slow = {
        let c = client.clone();
        tokio::spawn(async move { c.call_tool("slow", json!({"ms": 400}), None).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let started = Instant::now();
    let echo = client
        .call_tool("echo", json!({"text": "fast"}), None)
        .await
        .unwrap();
    // The echo came back while the slow call was still running.
    assert_eq!(echo.text, "fast");
    assert!(started.elapsed() < Duration::from_millis(300));
    assert_eq!(slow.await.unwrap().unwrap().text, "done");
}

#[tokio::test]
async fn requests_time_out_and_can_be_cancelled() {
    let client = McpClient::connect(&fixture("fixture", Duration::from_millis(200)), None)
        .await
        .unwrap();
    assert!(matches!(
        client.call_tool("slow", json!({"ms": 3000}), None).await,
        Err(McpError::Timeout(_))
    ));

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    let client = connect().await;
    let started = Instant::now();
    assert!(matches!(
        client
            .call_tool("slow", json!({"ms": 3000}), Some(&cancel))
            .await,
        Err(McpError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    // The connection is still usable afterwards.
    assert_eq!(
        client
            .call_tool("echo", json!({"text": "ok"}), None)
            .await
            .unwrap()
            .text,
        "ok"
    );
}

#[tokio::test]
async fn a_crashed_server_is_restarted_on_the_next_call() {
    let server = McpServer::new(fixture("fixture", Duration::from_secs(10)), None);
    let first = server.client().await.unwrap();
    assert!(first.call_tool("crash", json!({}), None).await.is_err());
    // Give the reader a moment to see the process exit.
    for _ in 0..50 {
        if first.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(first.is_closed());
    let second = server.client().await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        second
            .call_tool("echo", json!({"text": "back"}), None)
            .await
            .unwrap()
            .text,
        "back"
    );
}

#[tokio::test]
async fn server_stderr_goes_to_its_log_file() {
    let logs = tempfile::tempdir().unwrap();
    let client = McpClient::connect(
        &fixture("logged", Duration::from_secs(10)),
        Some(logs.path()),
    )
    .await
    .unwrap();
    client.list_tools().await.unwrap();
    let log = std::fs::read_to_string(logs.path().join("logged.log")).unwrap();
    assert!(log.contains("fixture server starting"), "{log}");
}

type NamedTools = Vec<(String, Arc<dyn ToolExecutor>)>;

/// Records what the manager registers.
#[derive(Default)]
struct RecordingSink {
    tools: Mutex<BTreeMap<String, NamedTools>>,
}

impl ToolSink for RecordingSink {
    fn replace_server_tools(&self, server: &str, tools: Vec<(String, Arc<dyn ToolExecutor>)>) {
        self.tools.lock().unwrap().insert(server.to_string(), tools);
    }
}

impl RecordingSink {
    fn names(&self, server: &str) -> Vec<String> {
        self.tools.lock().unwrap()[server]
            .iter()
            .map(|(n, _)| n.clone())
            .collect()
    }

    fn executor(&self, server: &str, name: &str) -> Arc<dyn ToolExecutor> {
        self.tools.lock().unwrap()[server]
            .iter()
            .find(|(n, _)| n == name)
            .unwrap()
            .1
            .clone()
    }
}

fn invocation(tool: &str, arguments: Value) -> ToolInvocation {
    ToolInvocation {
        id: ToolCallId::new(),
        source_turn: TurnId::new(),
        tool_name: tool.to_string(),
        arguments,
        risk: RiskLevel::Medium,
        rationale: None,
    }
}

#[tokio::test]
async fn the_manager_registers_tools_reports_failures_and_refreshes_changed_lists() {
    let broken = McpServerConfig {
        name: "broken".into(),
        transport: TransportConfig::Stdio {
            command: "definitely-not-a-real-program-arbe".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout: Duration::from_secs(5),
    };
    let manager = McpManager::new(vec![fixture("fx", Duration::from_secs(10)), broken], None);
    let sink = RecordingSink::default();
    let statuses = Mutex::new(Vec::new());
    manager
        .connect_all(&sink, &|s| statuses.lock().unwrap().push(s))
        .await;

    let statuses = statuses.into_inner().unwrap();
    assert!(statuses.contains(&ServerStatus::Connected {
        server: "fx".into(),
        tools: 6
    }));
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, ServerStatus::Failed { server, .. } if server == "broken"))
    );

    assert!(sink.names("fx").contains(&"fx__echo".to_string()));
    let echo = sink.executor("fx", "fx__echo");
    assert_eq!(echo.default_risk(), RiskLevel::Low);
    assert!(echo.parallel_safe());
    assert_eq!(echo.description().parameters["required"], json!(["text"]));
    assert_eq!(
        sink.executor("fx", "fx__crash").default_risk(),
        RiskLevel::High
    );
    assert!(!sink.executor("fx", "fx__add").parallel_safe());

    let result = echo
        .execute(
            invocation("fx__echo", json!({"text": "via registry"})),
            &ToolContext::for_testing(),
        )
        .await
        .unwrap();
    assert_eq!(result.output, json!("via registry"));
    let failed = sink
        .executor("fx", "fx__fail")
        .execute(
            invocation("fx__fail", json!({})),
            &ToolContext::for_testing(),
        )
        .await
        .unwrap();
    assert!(failed.is_error);

    // The server announces a changed tool list; a refresh picks it up.
    sink.executor("fx", "fx__toggle")
        .execute(
            invocation("fx__toggle", json!({})),
            &ToolContext::for_testing(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.refresh_changed(&sink).await;
    assert!(sink.names("fx").contains(&"fx__bonus".to_string()));
}

#[tokio::test]
async fn cancelling_a_tool_context_cancels_the_mcp_call() {
    let manager = McpManager::new(vec![fixture("fx", Duration::from_secs(10))], None);
    let sink = RecordingSink::default();
    manager.connect_all(&sink, &|_| {}).await;
    let ctx = ToolContext::for_testing();
    let cancel = ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    });
    let err = sink
        .executor("fx", "fx__slow")
        .execute(invocation("fx__slow", json!({"ms": 3000})), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Cancelled));
}

// ---------------------------------------------------------------------------
// Streamable HTTP
// ---------------------------------------------------------------------------

/// Serves one fixed-behavior MCP endpoint: JSON for `initialize` (with a
/// session id), SSE for `tools/list` (a notification, then the response),
/// JSON for `tools/call`, 202 for notifications. Records the session
/// header each request carried.
async fn http_fixture() -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let sessions = Arc::new(Mutex::new(Vec::new()));
    let seen = sessions.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                let (head, mut body) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..pos]).to_string();
                        break (head, raw[pos + 4..].to_vec());
                    }
                };
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.trim()
                            .eq_ignore_ascii_case(name)
                            .then(|| v.trim().to_string())
                    })
                };
                let length: usize = header("content-length").unwrap().parse().unwrap();
                while body.len() < length {
                    let n = socket.read(&mut buf).await.unwrap();
                    body.extend_from_slice(&buf[..n]);
                }
                seen.lock().unwrap().push(header("mcp-session-id"));
                let message: Value = serde_json::from_slice(&body).unwrap();
                let id = message.get("id").cloned();
                let (status, content_type, extra, payload) = match (message["method"].as_str(), id)
                {
                    (_, None) => ("202 Accepted", "application/json", "", String::new()),
                    (Some("initialize"), Some(id)) => (
                        "200 OK",
                        "application/json",
                        "Mcp-Session-Id: session-1\r\n",
                        json!({"jsonrpc": "2.0", "id": id, "result": {
                            "protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                            "serverInfo": {"name": "http-fixture", "version": "0"}}})
                        .to_string(),
                    ),
                    (Some("tools/list"), Some(id)) => (
                        "200 OK",
                        "text/event-stream",
                        "",
                        format!(
                            "event: message\ndata: {}\n\ndata: {}\n\n",
                            json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}}),
                            json!({"jsonrpc": "2.0", "id": id, "result": {"tools": [
                                {"name": "lookup", "inputSchema": {"type": "object"}}]}})
                        ),
                    ),
                    (_, Some(id)) => (
                        "200 OK",
                        "application/json",
                        "",
                        json!({"jsonrpc": "2.0", "id": id, "result": {
                            "content": [{"type": "text", "text": "looked up"}]}})
                        .to_string(),
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, sessions)
}

#[tokio::test]
async fn talks_streamable_http_with_json_and_sse_replies_and_a_session() {
    let (url, sessions) = http_fixture().await;
    let config = McpServerConfig {
        name: "remote".into(),
        transport: TransportConfig::Http {
            url,
            headers: BTreeMap::new(),
            bearer_token: None,
        },
        timeout: Duration::from_secs(10),
    };
    let client = McpClient::connect(&config, None).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools[0].name, "lookup");
    assert_eq!(
        client
            .call_tool("lookup", json!({}), None)
            .await
            .unwrap()
            .text,
        "looked up"
    );

    let sessions = sessions.lock().unwrap();
    // initialize carried no session; everything after it did.
    assert_eq!(sessions[0], None);
    assert!(
        sessions[1..]
            .iter()
            .all(|s| s.as_deref() == Some("session-1")),
        "{sessions:?}"
    );
}

/// Against the official reference server, run through `npx` (needs Node
/// and network on first run; on Windows this also exercises the `.cmd`
/// fallback). Run with:
/// `cargo test -p arbe-mcp --test client -- --ignored --nocapture`
#[tokio::test]
#[ignore = "needs Node/npx and network"]
async fn works_with_the_official_reference_server() {
    let config = McpServerConfig {
        name: "everything".into(),
        transport: TransportConfig::Stdio {
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-everything".into(),
            ],
            env: BTreeMap::new(),
            cwd: None,
        },
        timeout: Duration::from_secs(120),
    };
    let client = McpClient::connect(&config, None)
        .await
        .expect("server-everything starts");
    let tools = client.list_tools().await.unwrap();
    eprintln!(
        "tools: {:?}",
        tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>()
    );
    assert!(tools.iter().any(|t| t.name == "echo"));
    let echo = client
        .call_tool("echo", json!({"message": "hello from arbe"}), None)
        .await
        .unwrap();
    eprintln!("echo: {echo:?}");
    assert!(echo.text.contains("hello from arbe"));
    assert!(!echo.is_error);
    // Tool names change between releases; check the ones present.
    let has = |name: &str| tools.iter().any(|t| t.name == name);
    if has("get-sum") {
        let sum = client
            .call_tool("get-sum", json!({"a": 2, "b": 3}), None)
            .await
            .unwrap();
        eprintln!("get-sum: {sum:?}");
        assert!(sum.text.contains('5'));
    }
    if has("get-tiny-image") {
        let image = client
            .call_tool("get-tiny-image", json!({}), None)
            .await
            .unwrap();
        eprintln!("get-tiny-image: {image:?}");
        assert!(image.text.contains("[image: image/png]"));
    }
    if has("get-structured-content") {
        let structured = client
            .call_tool(
                "get-structured-content",
                json!({"location": "New York"}),
                None,
            )
            .await
            .unwrap();
        eprintln!("get-structured-content: {structured:?}");
        assert!(!structured.text.is_empty());
    }
}
