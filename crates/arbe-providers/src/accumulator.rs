use arbe_core::{ContentBlock, Message, Role, StopReason, Usage};
use serde_json::Value;

use crate::ProviderEvent;

/// A block still being streamed.
#[derive(Debug)]
enum Partial {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
    Done(ContentBlock),
}

/// Folds a stream of [`ProviderEvent`]s into the final assistant
/// [`Message`], [`Usage`] and [`StopReason`]. Pure and synchronous, so it
/// can be tested against arbitrary event sequences without any network.
///
/// Blocks keep the order they started in. Consecutive text deltas extend
/// the current text block, and a text delta after a tool use starts a new
/// text block, so interleavings are preserved rather than merged.
#[derive(Debug, Default)]
pub struct ResponseAccumulator {
    blocks: Vec<Partial>,
    usage: Usage,
    stop: Option<StopReason>,
}

/// What an accumulated response turned into.
#[derive(Debug, Clone)]
pub struct AccumulatedResponse {
    pub message: Message,
    pub usage: Usage,
    pub stop_reason: StopReason,
}

impl ResponseAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: ProviderEvent) {
        match event {
            ProviderEvent::TextDelta(delta) => match self.blocks.last_mut() {
                Some(Partial::Text(text)) => text.push_str(&delta),
                _ => self.blocks.push(Partial::Text(delta)),
            },
            ProviderEvent::ThinkingDelta(delta) => match self.blocks.last_mut() {
                Some(Partial::Thinking { text, .. }) => text.push_str(&delta),
                _ => self.blocks.push(Partial::Thinking {
                    text: delta,
                    signature: None,
                }),
            },
            ProviderEvent::ThinkingSignature(sig) => {
                let current = self.blocks.iter_mut().rev().find_map(|b| match b {
                    Partial::Thinking { signature, .. } => Some(signature),
                    _ => None,
                });
                match current {
                    Some(signature) => signature.get_or_insert_with(String::new).push_str(&sig),
                    None => self.blocks.push(Partial::Thinking {
                        text: String::new(),
                        signature: Some(sig),
                    }),
                }
            }
            ProviderEvent::ToolUseStart { id, name } => self.blocks.push(Partial::ToolUse {
                id,
                name,
                json: String::new(),
            }),
            ProviderEvent::ToolUseInputDelta { id, partial_json } => {
                if let Some(json) = self.open_tool_use(&id) {
                    json.push_str(&partial_json);
                }
            }
            ProviderEvent::ToolUseEnd { id } => {
                let slot = self
                    .blocks
                    .iter_mut()
                    .find(|b| matches!(b, Partial::ToolUse { id: open, .. } if *open == id));
                if let Some(slot) = slot {
                    let block = std::mem::replace(slot, Partial::Text(String::new()));
                    *slot = Partial::Done(finish_block(block));
                }
            }
            ProviderEvent::Opaque { provider, data } => self
                .blocks
                .push(Partial::Done(ContentBlock::Opaque { provider, data })),
            ProviderEvent::Usage(usage) => merge_usage(&mut self.usage, usage),
            ProviderEvent::Stop(reason) => self.stop = Some(reason),
        }
    }

    fn open_tool_use(&mut self, id: &str) -> Option<&mut String> {
        self.blocks.iter_mut().find_map(|b| match b {
            Partial::ToolUse { id: open, json, .. } if open == id => Some(json),
            _ => None,
        })
    }

    /// Text accumulated so far (for progress display).
    pub fn text_so_far(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Partial::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Closes any still-open blocks and returns the result. Without an
    /// explicit `Stop` the reason is inferred: `ToolUse` if the message
    /// requests tools, otherwise `EndTurn`.
    pub fn finish(self) -> AccumulatedResponse {
        let content: Vec<ContentBlock> = self
            .blocks
            .into_iter()
            .map(finish_block)
            .filter(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))
            .collect();
        let message = Message::with_blocks(Role::Assistant, content);
        let stop_reason = self.stop.unwrap_or_else(|| {
            if message.has_tool_uses() {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            }
        });
        AccumulatedResponse {
            message,
            usage: self.usage,
            stop_reason,
        }
    }
}

fn finish_block(block: Partial) -> ContentBlock {
    match block {
        Partial::Text(text) => ContentBlock::Text { text },
        Partial::Thinking { text, signature } => ContentBlock::Thinking { text, signature },
        Partial::ToolUse { id, name, json } => ContentBlock::ToolUse {
            id,
            name,
            input: parse_tool_input(&json),
        },
        Partial::Done(block) => block,
    }
}

/// Parses a tool call's accumulated JSON arguments. Empty means "no
/// arguments" (`{}`). Malformed JSON is passed through as a string rather
/// than failing the whole response, so the tool's own argument validation
/// reports the problem with the model's original text visible.
pub fn parse_tool_input(json: &str) -> Value {
    if json.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(json).unwrap_or_else(|_| Value::String(json.to_string()))
}

/// Usage events carry the latest known cumulative counts, with 0 meaning
/// "not reported here" — so keep the larger value per field.
fn merge_usage(total: &mut Usage, update: Usage) {
    total.input_tokens = total.input_tokens.max(update.input_tokens);
    total.output_tokens = total.output_tokens.max(update.output_tokens);
    total.cache_read_tokens = total.cache_read_tokens.max(update.cache_read_tokens);
    total.cache_write_tokens = total.cache_write_tokens.max(update.cache_write_tokens);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(events: Vec<ProviderEvent>) -> AccumulatedResponse {
        let mut acc = ResponseAccumulator::new();
        for e in events {
            acc.push(e);
        }
        acc.finish()
    }

    fn text(s: &str) -> ProviderEvent {
        ProviderEvent::TextDelta(s.to_string())
    }

    fn start(id: &str, name: &str) -> ProviderEvent {
        ProviderEvent::ToolUseStart {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn input(id: &str, frag: &str) -> ProviderEvent {
        ProviderEvent::ToolUseInputDelta {
            id: id.to_string(),
            partial_json: frag.to_string(),
        }
    }

    #[test]
    fn concatenates_text_deltas_into_one_block() {
        let r = run(vec![
            text("Hel"),
            text("lo"),
            ProviderEvent::Stop(StopReason::EndTurn),
        ]);
        assert_eq!(r.message.content, vec![ContentBlock::text("Hello")]);
        assert_eq!(r.message.role, Role::Assistant);
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn reassembles_fragmented_tool_input_json() {
        let r = run(vec![
            start("c1", "read_file"),
            input("c1", "{\"pa"),
            input("c1", "th\": \"src/"),
            input("c1", "main.rs\"}"),
            ProviderEvent::ToolUseEnd { id: "c1".into() },
        ]);
        let calls = r.message.tool_uses();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, json!({"path": "src/main.rs"}));
        // Inferred without an explicit Stop.
        assert_eq!(r.stop_reason, StopReason::ToolUse);
    }

    #[test]
    fn interleaved_tool_uses_are_kept_apart_and_in_start_order() {
        let r = run(vec![
            start("a", "glob"),
            start("b", "grep"),
            input("b", "{\"pattern\":"),
            input("a", "{\"pattern\":\"*.rs\"}"),
            input("b", "\"fn\"}"),
        ]);
        let calls = r.message.tool_uses();
        assert_eq!(calls[0].name, "glob");
        assert_eq!(calls[0].arguments, json!({"pattern": "*.rs"}));
        assert_eq!(calls[1].name, "grep");
        assert_eq!(calls[1].arguments, json!({"pattern": "fn"}));
    }

    #[test]
    fn empty_input_is_an_empty_object_and_malformed_input_is_kept_as_a_string() {
        let r = run(vec![
            start("a", "list_dir"),
            start("b", "read_file"),
            input("b", "{oops"),
        ]);
        let calls = r.message.tool_uses();
        assert_eq!(calls[0].arguments, json!({}));
        assert_eq!(calls[1].arguments, json!("{oops"));
    }

    #[test]
    fn text_after_a_tool_use_starts_a_new_block() {
        let r = run(vec![
            text("Let me look."),
            start("c1", "list_dir"),
            text("Done."),
        ]);
        assert_eq!(r.message.content.len(), 3);
        assert_eq!(r.message.text(), "Let me look.Done.");
    }

    #[test]
    fn thinking_and_its_signature_form_one_block_before_the_text() {
        let r = run(vec![
            ProviderEvent::ThinkingDelta("step 1, ".into()),
            ProviderEvent::ThinkingDelta("step 2".into()),
            ProviderEvent::ThinkingSignature("sig".into()),
            text("answer"),
        ]);
        assert_eq!(
            r.message.content,
            vec![
                ContentBlock::Thinking {
                    text: "step 1, step 2".into(),
                    signature: Some("sig".into())
                },
                ContentBlock::text("answer"),
            ]
        );
    }

    #[test]
    fn usage_keeps_the_latest_reported_value_per_field_in_any_order() {
        let r = run(vec![
            ProviderEvent::Usage(Usage {
                input_tokens: 100,
                output_tokens: 1,
                ..Default::default()
            }),
            ProviderEvent::Stop(StopReason::EndTurn),
            ProviderEvent::Usage(Usage {
                output_tokens: 42,
                ..Default::default()
            }),
        ]);
        assert_eq!(r.usage.input_tokens, 100);
        assert_eq!(r.usage.output_tokens, 42);
    }

    #[test]
    fn explicit_stop_reason_wins_over_inference() {
        let r = run(vec![
            start("c1", "x"),
            ProviderEvent::Stop(StopReason::MaxTokens),
        ]);
        assert_eq!(r.stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn deltas_for_an_unknown_tool_id_are_ignored() {
        let r = run(vec![input("ghost", "{}"), text("hi")]);
        assert_eq!(r.message.content, vec![ContentBlock::text("hi")]);
    }

    /// Property-style check: however a tool call's JSON is split into
    /// fragments, the reassembled arguments are identical.
    #[test]
    fn any_fragmentation_of_tool_input_reassembles_identically() {
        let full =
            r#"{"path":"src/ünïcode/main.rs","content":"line1\nline2 \"quoted\"","n":[1,2,3]}"#;
        let expected: Value = serde_json::from_str(full).unwrap();
        let chars: Vec<char> = full.chars().collect();
        for chunk_size in 1..=chars.len() {
            let mut events = vec![start("c", "write_file")];
            for piece in chars.chunks(chunk_size) {
                events.push(input("c", &piece.iter().collect::<String>()));
            }
            let r = run(events);
            assert_eq!(
                r.message.tool_uses()[0].arguments,
                expected,
                "chunk size {chunk_size}"
            );
        }
    }
}
