//! The `ask_user` tool (v2 plan P6.5): the model asks the user a question —
//! optionally with options to pick from — and the turn waits for the
//! answer, the way it waits for an approval.
//!
//! The question goes out as `RuntimeEvent::UserQuestionAsked`; a UI answers
//! with `Agent::answer_question(question_id, text)`. Like approval
//! decisions, the mailbox is shared by a whole tree of subagents, so the
//! top-level agent answers a subagent's question too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arbe_core::{RiskLevel, RuntimeEvent, ToolCallId, ToolError, ToolInvocation, ToolResult};
use arbe_tools::{ToolContext, ToolDescription, ToolExecutor, schemars};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;

use crate::EventBus;

pub(super) const ASK_USER_TOOL: &str = "ask_user";

/// Most options a question may offer.
const MAX_OPTIONS: usize = 8;

/// Answers to questions that are waiting for one.
#[derive(Default)]
pub(super) struct QuestionMailbox {
    waiting: Mutex<HashMap<ToolCallId, oneshot::Sender<String>>>,
}

impl QuestionMailbox {
    fn register(&self, id: ToolCallId) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id, tx);
        rx
    }

    /// Delivers `answer` to the question `id`. `false` if nothing is
    /// waiting on it (already answered, or the turn was cancelled).
    pub(super) fn answer(&self, id: ToolCallId, answer: String) -> bool {
        match self.lock().remove(&id) {
            Some(tx) => tx.send(answer).is_ok(),
            None => false,
        }
    }

    fn withdraw(&self, id: ToolCallId) {
        self.lock().remove(&id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ToolCallId, oneshot::Sender<String>>> {
        self.waiting.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct Args {
    /// The question, complete and self-contained.
    question: String,
    /// Optional answers to choose from (at most 8), e.g. the approaches
    /// you're deciding between.
    #[serde(default)]
    options: Vec<String>,
    /// Whether the user may answer something other than the options
    /// (default true; always true without options).
    #[serde(default)]
    allow_free_text: Option<bool>,
}

pub(super) struct AskUserTool {
    mailbox: Arc<QuestionMailbox>,
    events: Arc<EventBus>,
}

impl AskUserTool {
    pub(super) fn new(mailbox: Arc<QuestionMailbox>, events: Arc<EventBus>) -> Self {
        Self { mailbox, events }
    }
}

#[async_trait]
impl ToolExecutor for AskUserTool {
    fn read_only(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid ask_user arguments: {e}")))?;
        let question = args.question.trim().to_string();
        if question.is_empty() {
            return Err(ToolError::Validation("question is empty".into()));
        }
        let options: Vec<String> = args
            .options
            .into_iter()
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect();
        if options.len() > MAX_OPTIONS {
            return Err(ToolError::Validation(format!(
                "at most {MAX_OPTIONS} options"
            )));
        }
        let allow_free_text = options.is_empty() || args.allow_free_text.unwrap_or(true);

        let id = invocation.id;
        let answer = self.mailbox.register(id);
        self.events.publish(RuntimeEvent::UserQuestionAsked {
            turn_id: invocation.source_turn,
            question_id: id,
            question,
            options,
            allow_free_text,
        });
        let answer = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                self.mailbox.withdraw(id);
                return Err(ToolError::Cancelled);
            }
            answer = answer => answer.map_err(|_| ToolError::Cancelled)?,
        };
        Ok(ToolResult {
            id,
            output: json!({ "answer": answer }),
            is_error: false,
            attachments: Vec::new(),
        })
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Ask the user a question and wait for the answer. Use it only when you need a decision or information that only the user has — which of several reasonable approaches they want, a missing requirement, a credential-free detail you can't find. Don't ask what you can find out yourself with your tools, and don't ask for permission to use tools (the harness does that). Offer options when the answer is a choice.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn parallel_safe(&self) -> bool {
        // One question at a time, and the round marks the session as
        // waiting for an answer while it runs.
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arbe_core::TurnId;
    use serde_json::Value;

    fn call(args: Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: ASK_USER_TOOL.into(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[tokio::test]
    async fn a_question_is_published_and_its_answer_is_the_result() {
        let mailbox = Arc::new(QuestionMailbox::default());
        let events = Arc::new(EventBus::new(16));
        let mut rx = events.subscribe();
        let tool = AskUserTool::new(mailbox.clone(), events);
        let invocation = call(
            json!({"question": "Tabs or spaces?", "options": ["tabs", " spaces ", ""], "allow_free_text": false}),
        );
        let id = invocation.id;
        let run =
            tokio::spawn(
                async move { tool.execute(invocation, &ToolContext::for_testing()).await },
            );
        match rx.recv().await.unwrap().event {
            RuntimeEvent::UserQuestionAsked {
                question_id,
                question,
                options,
                allow_free_text,
                ..
            } => {
                assert_eq!(question_id, id);
                assert_eq!(question, "Tabs or spaces?");
                assert_eq!(options, ["tabs", "spaces"]);
                assert!(!allow_free_text);
            }
            other => panic!("{other:?}"),
        }
        assert!(mailbox.answer(id, "spaces".into()));
        assert!(!mailbox.answer(id, "again".into()));
        let result = run.await.unwrap().unwrap();
        assert_eq!(result.output, json!({"answer": "spaces"}));
    }

    #[tokio::test]
    async fn cancelling_withdraws_the_question() {
        let mailbox = Arc::new(QuestionMailbox::default());
        let tool = AskUserTool::new(mailbox.clone(), Arc::new(EventBus::new(16)));
        let invocation = call(json!({"question": "?"}));
        let id = invocation.id;
        let ctx = ToolContext::for_testing();
        let cancel = ctx.cancel.clone();
        let run = tokio::spawn(async move { tool.execute(invocation, &ctx).await });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(matches!(run.await.unwrap(), Err(ToolError::Cancelled)));
        assert!(!mailbox.answer(id, "late".into()));
    }

    #[tokio::test]
    async fn bad_questions_are_rejected() {
        let tool = AskUserTool::new(Default::default(), Arc::new(EventBus::new(16)));
        for args in [
            json!({"question": "  "}),
            json!({"question": "q", "options": ["1","2","3","4","5","6","7","8","9"]}),
        ] {
            assert!(
                tool.execute(call(args), &ToolContext::for_testing())
                    .await
                    .is_err()
            );
        }
    }
}
