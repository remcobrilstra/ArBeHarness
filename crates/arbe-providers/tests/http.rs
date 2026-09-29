//! Every adapter end to end over real HTTP (v2 plan P7.1): request shape,
//! streaming, status-code mapping, `Retry-After`, malformed data, dropped
//! connections and cancellation. `wiremock` serves whole responses; the
//! cases it can't express (a connection cut mid-body, a stream that never
//! ends) use a raw TCP server.

use std::time::{Duration, Instant};

use arbe_core::{Message, ProviderError, Role, StopReason};
use arbe_providers::anthropic::AnthropicProvider;
use arbe_providers::ollama::OllamaProvider;
use arbe_providers::openai::OpenAiProvider;
use arbe_providers::{
    CancellationToken, ModelProvider, ModelRequest, RetryPolicy, infer, stream_with_retry,
};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The three wire formats under test, each with a canned streamed reply
/// saying "Hello" and where it's served.
#[derive(Clone, Copy, Debug)]
enum Wire {
    OpenAi,
    Anthropic,
    Ollama,
}

const WIRES: [Wire; 3] = [Wire::OpenAi, Wire::Anthropic, Wire::Ollama];

impl Wire {
    fn provider(self, base_url: &str) -> Box<dyn ModelProvider> {
        match self {
            Wire::OpenAi => Box::new(OpenAiProvider::new("test-key").with_base_url(base_url)),
            Wire::Anthropic => Box::new(AnthropicProvider::new("test-key").with_base_url(base_url)),
            Wire::Ollama => Box::new(OllamaProvider::new().with_base_url(base_url)),
        }
    }

    fn path(self) -> &'static str {
        match self {
            Wire::OpenAi => "/chat/completions",
            Wire::Anthropic => "/v1/messages",
            Wire::Ollama => "/api/chat",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Wire::Ollama => "application/x-ndjson",
            _ => "text/event-stream",
        }
    }

    /// A complete streamed reply: "Hel" + "lo", usage, a normal stop.
    fn hello(self) -> String {
        match self {
            Wire::OpenAi => [
                r#"data: {"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
                r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
                r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
                r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2}}"#,
                "data: [DONE]",
            ]
            .map(|l| format!("{l}\n\n"))
            .concat(),
            Wire::Anthropic => [
                ("message_start", r#"{"type":"message_start","message":{"usage":{"input_tokens":7,"output_tokens":0}}}"#),
                ("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
                ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}"#),
                ("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lo"}}"#),
                ("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
                ("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#),
                ("message_stop", r#"{"type":"message_stop"}"#),
            ]
            .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
            .concat(),
            Wire::Ollama => [
                r#"{"message":{"role":"assistant","content":"Hel"},"done":false}"#,
                r#"{"message":{"role":"assistant","content":"lo"},"done":false}"#,
                r#"{"message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":7,"eval_count":2}"#,
            ]
            .map(|l| format!("{l}\n"))
            .concat(),
        }
    }

    /// The first part of [`hello`](Self::hello): some text, no ending.
    fn hello_cut_short(self) -> String {
        let full = self.hello();
        let marker = match self {
            Wire::OpenAi => "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"",
            Wire::Anthropic => {
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\""
            }
            Wire::Ollama => "{\"message\":{\"role\":\"assistant\",\"content\":\"lo\"",
        };
        full[..full.find(marker).unwrap()].to_string()
    }

    /// One line of garbage in the wire format.
    fn malformed(self) -> String {
        match self {
            Wire::Ollama => "{not json\n".into(),
            Wire::OpenAi => "data: {not json\n\n".into(),
            Wire::Anthropic => "event: content_block_delta\ndata: {not json\n\n".into(),
        }
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        model: "test-model".into(),
        messages: vec![Message::new(Role::User, "hi")],
        temperature: 0.0,
        max_tokens: 100,
        tools: Vec::new(),
        thinking_budget_tokens: None,
    }
}

fn streamed(wire: Wire, body: String) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.into_bytes(), wire.content_type())
}

async fn server_replying(wire: Wire, response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(wire.path()))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_streamed_reply_arrives_whole_with_usage() {
    for wire in WIRES {
        let server = server_replying(wire, streamed(wire, wire.hello())).await;
        let provider = wire.provider(&server.uri());
        let response = infer(provider.as_ref(), request(), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("{wire:?}: {e}"));
        assert_eq!(response.text(), "Hello", "{wire:?}");
        assert_eq!(response.stop_reason, StopReason::EndTurn, "{wire:?}");
        assert_eq!(response.usage.input_tokens, 7, "{wire:?}");
        assert_eq!(response.usage.output_tokens, 2, "{wire:?}");
    }
}

#[tokio::test]
async fn requests_carry_the_model_and_credentials() {
    for wire in WIRES {
        let server = MockServer::start().await;
        let auth = match wire {
            Wire::OpenAi => Some(("authorization", "Bearer test-key")),
            Wire::Anthropic => Some(("x-api-key", "test-key")),
            Wire::Ollama => None,
        };
        let mut mock = Mock::given(method("POST")).and(path(wire.path()));
        if let Some((name, value)) = auth {
            mock = mock.and(header(name, value));
        }
        mock.respond_with(streamed(wire, wire.hello()))
            .expect(1)
            .mount(&server)
            .await;
        infer(
            wire.provider(&server.uri()).as_ref(),
            request(),
            CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("{wire:?}: {e}"));

        let received = server.received_requests().await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(body["model"], "test-model", "{wire:?}");
        assert_eq!(body["stream"], true, "{wire:?}");
    }
}

#[tokio::test]
async fn status_codes_map_to_their_errors() {
    for wire in WIRES {
        for (status, check) in [
            (401, "auth"),
            (429, "rate limit"),
            (500, "retryable"),
            (503, "retryable"),
            (529, "retryable"),
        ] {
            let server = server_replying(
                wire,
                ResponseTemplate::new(status).set_body_string(r#"{"error":{"message":"nope"}}"#),
            )
            .await;
            let err = infer(
                wire.provider(&server.uri()).as_ref(),
                request(),
                CancellationToken::new(),
            )
            .await
            .expect_err("should fail");
            let ok = match check {
                "auth" => matches!(err, ProviderError::Auth(_)) && !err.is_retryable(),
                "rate limit" => matches!(err, ProviderError::RateLimit { .. }),
                _ => err.is_retryable(),
            };
            assert!(ok, "{wire:?} {status}: {err:?}");
        }
    }
}

#[tokio::test]
async fn a_context_overflow_is_recognized() {
    let cases = [
        (
            Wire::OpenAi,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#,
        ),
        (
            Wire::Anthropic,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#,
        ),
    ];
    for (wire, body) in cases {
        let server = server_replying(wire, ResponseTemplate::new(400).set_body_string(body)).await;
        let err = infer(
            wire.provider(&server.uri()).as_ref(),
            request(),
            CancellationToken::new(),
        )
        .await
        .expect_err("should fail");
        assert!(
            matches!(err, ProviderError::ContextLengthExceeded(_)),
            "{wire:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn retry_after_is_honored_then_the_retry_succeeds() {
    for wire in WIRES {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(wire.path()))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(wire.path()))
            .respond_with(streamed(wire, wire.hello()))
            .mount(&server)
            .await;

        let provider = wire.provider(&server.uri());
        let policy = RetryPolicy {
            max_retries: 2,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_secs(5),
        };
        let mut notices = Vec::new();
        let started = Instant::now();
        let mut stream = stream_with_retry(
            provider.as_ref(),
            request(),
            &CancellationToken::new(),
            &policy,
            |notice| notices.push(notice.delay),
        )
        .await
        .unwrap_or_else(|e| panic!("{wire:?}: {e}"));
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let arbe_providers::ProviderEvent::TextDelta(delta) = event.unwrap() {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "Hello", "{wire:?}");
        assert_eq!(notices.len(), 1, "{wire:?}");
        // The server's Retry-After, not the policy's 10 ms base delay.
        assert!(
            notices[0] >= Duration::from_secs(1),
            "{wire:?}: {notices:?}"
        );
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn a_malformed_chunk_is_an_error_not_silence() {
    for wire in WIRES {
        let body = wire.hello_cut_short() + &wire.malformed();
        let server = server_replying(wire, streamed(wire, body)).await;
        let result = infer(
            wire.provider(&server.uri()).as_ref(),
            request(),
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_err(), "{wire:?}: {result:?}");
    }
}

/// A one-connection HTTP server that sends a 200 streaming response head
/// and `body` (chunked), then either drops the connection mid-body or
/// keeps it open without sending more.
async fn raw_server(wire: Wire, body: String, then_hang: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read (and ignore) the request.
        let mut buf = vec![0u8; 65_536];
        let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {}\r\ntransfer-encoding: chunked\r\n\r\n",
            wire.content_type()
        );
        let chunk = format!("{:x}\r\n{body}\r\n", body.len());
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(chunk.as_bytes()).await;
        let _ = socket.flush().await;
        if then_hang {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        // Dropped without the terminating chunk: the body is cut short.
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_connection_dropped_mid_answer_is_an_error_not_a_short_answer() {
    for wire in WIRES {
        let url = raw_server(wire, wire.hello_cut_short(), false).await;
        let result = infer(
            wire.provider(&url).as_ref(),
            request(),
            CancellationToken::new(),
        )
        .await;
        assert!(
            result.is_err(),
            "{wire:?}: a cut-off stream was accepted as {:?}",
            result.map(|r| r.text())
        );
    }
}

#[tokio::test]
async fn cancelling_a_stalled_stream_returns_promptly() {
    for wire in WIRES {
        let url = raw_server(wire, wire.hello_cut_short(), true).await;
        let provider = wire.provider(&url);
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            canceller.cancel();
        });
        let started = Instant::now();
        let result = infer(provider.as_ref(), request(), cancel).await;
        assert!(
            matches!(result, Err(ProviderError::Cancelled)),
            "{wire:?}: {result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{wire:?}");
    }
}

#[tokio::test]
async fn a_response_that_ends_without_its_end_marker_is_an_error() {
    for wire in WIRES {
        // A complete HTTP response whose stream just stops: no `[DONE]`,
        // `message_stop` or `"done": true` — e.g. a proxy giving up.
        let server = server_replying(wire, streamed(wire, wire.hello_cut_short())).await;
        let result = infer(
            wire.provider(&server.uri()).as_ref(),
            request(),
            CancellationToken::new(),
        )
        .await;
        assert!(
            result.is_err(),
            "{wire:?}: an unfinished stream was accepted as {:?}",
            result.map(|r| r.text())
        );
    }
}

#[tokio::test]
async fn done_without_a_finish_reason_still_completes() {
    // Some OpenAI-compatible servers end with `[DONE]` alone.
    let body = [
        r#"data: {"choices":[{"delta":{"role":"assistant","content":"Hi"}}]}"#,
        "data: [DONE]",
    ]
    .map(|l| format!("{l}\n\n"))
    .concat();
    let server = server_replying(Wire::OpenAi, streamed(Wire::OpenAi, body)).await;
    let response = infer(
        Wire::OpenAi.provider(&server.uri()).as_ref(),
        request(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(response.text(), "Hi");
    assert_eq!(response.stop_reason, StopReason::EndTurn);
}
