//! Streaming through a real adapter over local HTTP: how fast a long
//! reply is decoded, and how long until the first token reaches the
//! caller (the harness's own overhead, not the model's).
//!
//! `cargo bench -p arbe-providers`

use arbe_core::{Message, Role};
use arbe_providers::{CancellationToken, ModelProvider, ModelRequest, OpenAiProvider, infer};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use futures_util::StreamExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const DELTAS: usize = 5_000;

fn request() -> ModelRequest {
    ModelRequest {
        model: "bench".into(),
        messages: vec![Message::new(Role::User, "hi")],
        temperature: 0.0,
        max_tokens: 100,
        tools: Vec::new(),
        thinking_budget_tokens: None,
    }
}

/// An OpenAI-style SSE body of `DELTAS` small text deltas.
fn body() -> String {
    let mut body = String::new();
    for i in 0..DELTAS {
        body.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"token{i} \"}}}}]}}\n\n"
        ));
    }
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    body.push_str("data: [DONE]\n\n");
    body
}

fn streaming(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let body = body();
    let bytes = body.len() as u64;
    let server = rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;
        server
    });
    let provider = OpenAiProvider::new("key").with_base_url(server.uri());

    let mut group = c.benchmark_group("openai_stream");
    group.throughput(Throughput::Bytes(bytes));
    group.bench_function(format!("decode_{DELTAS}_deltas"), |b| {
        b.to_async(&rt).iter(|| async {
            let response = infer(&provider, request(), CancellationToken::new())
                .await
                .unwrap();
            assert!(!response.text().is_empty());
        })
    });
    group.finish();

    c.bench_function("openai_first_token", |b| {
        b.to_async(&rt).iter(|| async {
            let mut stream = provider
                .stream(request(), CancellationToken::new())
                .await
                .unwrap();
            stream.next().await.unwrap().unwrap()
        })
    });
}

criterion_group!(benches, streaming);
criterion_main!(benches);
