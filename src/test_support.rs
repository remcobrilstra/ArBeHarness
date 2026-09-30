//! A scripted model provider for the binary's own tests (`headless`,
//! `print`): no network, deterministic turns.

use arbe_tui::arbe_runtime::Harness;
use arbe_tui::arbe_runtime::arbe_core::{ProviderError, Role, StopReason, Usage};
use arbe_tui::arbe_runtime::arbe_providers::{
    CancellationToken, ModelCapabilities, ModelProvider, ModelRequest, ProviderEvent,
    ProviderStream,
};
use async_trait::async_trait;

/// Asks to read `a.txt`, then says whether the read returned its text.
pub struct ReadThenAnswer;

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

/// A harness on a temporary home and project (which holds `a.txt`) whose
/// model is [`ReadThenAnswer`]. Keep the directories alive for the test.
pub fn scripted_harness() -> (Harness, tempfile::TempDir, tempfile::TempDir) {
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
    (harness, home, project)
}
