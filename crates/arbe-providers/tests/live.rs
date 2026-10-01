//! Live smoke tests against real provider APIs (v2 plan P7.2).
//!
//! `#[ignore]`d so normal test runs never touch the network or spend
//! money. Run them with:
//!
//! ```text
//! cargo test -p arbe-providers --test live -- --ignored --nocapture
//! ```
//!
//! Each provider runs only when its credentials are present, and reports
//! "skipped" otherwise:
//! - OpenAI: `OPENAI_API_KEY` (model: `ARBE_LIVE_OPENAI_MODEL`, default `gpt-5-mini`)
//! - Anthropic: `ANTHROPIC_API_KEY` (model: `ARBE_LIVE_ANTHROPIC_MODEL`,
//!   default `claude-haiku-4-5-20251001`)
//! - Ollama: `ARBE_LIVE_OLLAMA=1` with a local server (model:
//!   `ARBE_LIVE_OLLAMA_MODEL`, default `qwen2.5-coder:7b`; `ARBE_BASE_URL` overrides
//!   the server address)
//! - Any OpenAI-compatible server (e.g. xAI): `ARBE_LIVE_COMPAT_BASE_URL`
//!   (e.g. `https://api.x.ai/v1`), `ARBE_LIVE_COMPAT_MODEL`, and
//!   `ARBE_LIVE_COMPAT_API_KEY` if the server needs one

use arbe_core::{Message, Role, StopReason, ToolSpec};
use arbe_providers::{
    CancellationToken, ModelProvider, ModelRequest, ProviderRegistry, ProviderSettings, infer,
};
use serde_json::json;

struct Target {
    provider: Box<dyn ModelProvider>,
    model: String,
    /// OpenAI reasoning models only accept the default (1.0); everything
    /// else runs deterministic, which small local models need to follow
    /// "reply with exactly..." reliably.
    temperature: f32,
}

fn target(name: &str) -> Option<Target> {
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let (api_key, model) = match name {
        "openai" => (
            Some(env("OPENAI_API_KEY")?),
            env("ARBE_LIVE_OPENAI_MODEL").unwrap_or_else(|| "gpt-5-mini".into()),
        ),
        "anthropic" => (
            Some(env("ANTHROPIC_API_KEY")?),
            env("ARBE_LIVE_ANTHROPIC_MODEL").unwrap_or_else(|| "claude-haiku-4-5-20251001".into()),
        ),
        "openai_compatible" => {
            env("ARBE_LIVE_COMPAT_BASE_URL")?;
            (
                env("ARBE_LIVE_COMPAT_API_KEY"),
                env("ARBE_LIVE_COMPAT_MODEL")?,
            )
        }
        "ollama" => {
            env("ARBE_LIVE_OLLAMA")?;
            (
                None,
                env("ARBE_LIVE_OLLAMA_MODEL").unwrap_or_else(|| "qwen2.5-coder:7b".into()),
            )
        }
        _ => unreachable!(),
    };
    let base_url = match name {
        "ollama" => env("ARBE_BASE_URL"),
        "openai_compatible" => env("ARBE_LIVE_COMPAT_BASE_URL"),
        _ => None,
    };
    Some(Target {
        provider: ProviderRegistry::with_builtins()
            .build(
                name,
                ProviderSettings {
                    api_key,
                    base_url,
                    ..Default::default()
                },
            )
            .expect("provider builds"),
        model,
        temperature: if name == "openai" { 1.0 } else { 0.0 },
    })
}

fn request(target: &Target, messages: Vec<Message>, tools: Vec<ToolSpec>) -> ModelRequest {
    ModelRequest {
        model: target.model.clone(),
        messages,
        temperature: target.temperature,
        max_tokens: 2_000,
        tools,
        thinking_budget_tokens: None,
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "Get the current weather for a city.".into(),
        parameters: json!({
            "type": "object",
            "properties": { "city": { "type": "string", "description": "City name" } },
            "required": ["city"],
        }),
    }
}

async fn text_round_trip(name: &str) {
    let Some(target) = target(name) else {
        eprintln!("{name}: skipped (no credentials configured)");
        return;
    };
    let response = infer(
        target.provider.as_ref(),
        request(
            &target,
            vec![
                Message::new(Role::System, "You follow instructions exactly."),
                Message::new(Role::User, "Say the word pong and nothing else."),
            ],
            vec![],
        ),
        CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{name}: request failed: {e}"));

    eprintln!("{name}: {:?} {:?}", response.text(), response.usage);
    assert!(
        response.text().to_lowercase().contains("pong"),
        "{name}: unexpected reply {:?}",
        response.text()
    );
    assert_eq!(response.stop_reason, StopReason::EndTurn);
    assert!(
        response.usage.output_tokens > 0,
        "{name}: no usage reported"
    );
}

async fn tool_round_trip(name: &str) {
    let Some(target) = target(name) else {
        eprintln!("{name}: skipped (no credentials configured)");
        return;
    };
    let mut messages = vec![Message::new(
        Role::User,
        "What's the weather in Paris right now? Use the get_weather tool.",
    )];

    let first = infer(
        target.provider.as_ref(),
        request(&target, messages.clone(), vec![weather_tool()]),
        CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{name}: first request failed: {e}"));
    let calls = first.tool_calls();
    eprintln!("{name}: tool calls {calls:?}");
    assert_eq!(calls.len(), 1, "{name}: expected one tool call");
    assert_eq!(calls[0].name, "get_weather");
    assert!(
        calls[0].arguments["city"]
            .as_str()
            .unwrap_or_default()
            .contains("Paris"),
        "{name}: unexpected arguments {:?}",
        calls[0].arguments
    );
    assert_eq!(first.stop_reason, StopReason::ToolUse);

    // Send the result back, echoing the assistant message verbatim (this
    // exercises thinking-signature and tool-id round-tripping).
    messages.push(first.message.clone());
    messages.push(Message::tool_result(
        calls[0].id.clone(),
        r#"{"temperature_c": 18, "conditions": "light rain"}"#,
    ));
    let second = infer(
        target.provider.as_ref(),
        request(&target, messages, vec![weather_tool()]),
        CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|e| panic!("{name}: follow-up request failed: {e}"));
    eprintln!("{name}: final {:?}", second.text());
    assert!(
        second.tool_calls().is_empty(),
        "{name}: expected a final answer"
    );
    assert!(
        second.text().contains("18") || second.text().to_lowercase().contains("rain"),
        "{name}: answer ignores the tool result: {:?}",
        second.text()
    );
}

#[tokio::test]
#[ignore = "hits the real OpenAI API"]
async fn openai_live() {
    text_round_trip("openai").await;
    tool_round_trip("openai").await;
}

#[tokio::test]
#[ignore = "hits the real Anthropic API"]
async fn anthropic_live() {
    text_round_trip("anthropic").await;
    tool_round_trip("anthropic").await;
}

#[tokio::test]
#[ignore = "needs a local Ollama server"]
async fn ollama_live() {
    text_round_trip("ollama").await;
    tool_round_trip("ollama").await;
}

#[tokio::test]
#[ignore = "hits a real OpenAI-compatible server"]
async fn openai_compatible_live() {
    text_round_trip("openai_compatible").await;
    tool_round_trip("openai_compatible").await;
}
