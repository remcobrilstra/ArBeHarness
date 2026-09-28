//! Tool calls a model wrote as text instead of through the API's tool-call
//! channel. Some small open models (notably the qwen2.5-coder family) reply
//! to a tool request with `{"name": "...", "arguments": {...}}` as plain
//! text — without the `<tool_call>` tags their own chat template asks for —
//! so the server's parser doesn't recognize it and the harness would show
//! JSON instead of running the tool.
//!
//! [`TextToolCallFilter`] sits on a stream of text deltas. While the reply
//! so far could still be a textual tool call it holds the text back; as
//! soon as it can't, it releases everything and passes text straight
//! through. At the end of the stream, held text that parses as one or more
//! calls to offered tools becomes tool calls; anything else is released as
//! the text it was. Only a reply that is *entirely* tool calls qualifies:
//! JSON inside prose is left alone, since it's usually an example.

use serde_json::Value;

/// How a textual tool call can start (after leading whitespace).
const OPENERS: &[&str] = &["{", "[", "<tool_call>", "```"];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug)]
pub(crate) struct TextToolCallFilter {
    offered: Vec<String>,
    held: String,
    /// Once text is released, the reply isn't a textual tool call.
    passthrough: bool,
}

impl TextToolCallFilter {
    /// A filter for a request that offered these tools. With none offered
    /// it never holds anything back.
    pub(crate) fn new(offered: Vec<String>) -> Self {
        let passthrough = offered.is_empty();
        Self {
            offered,
            held: String::new(),
            passthrough,
        }
    }

    /// Feeds a text delta; returns the text that can be shown now.
    pub(crate) fn push(&mut self, delta: &str) -> String {
        if self.passthrough {
            return delta.to_string();
        }
        self.held.push_str(delta);
        let start = self.held.trim_start();
        let could_be_call = start.is_empty()
            || OPENERS
                .iter()
                .any(|o| start.starts_with(o) || o.starts_with(start));
        if could_be_call {
            String::new()
        } else {
            self.passthrough = true;
            std::mem::take(&mut self.held)
        }
    }

    /// Ends the stream: the held text as tool calls if it is entirely
    /// calls to offered tools, otherwise as text to show.
    pub(crate) fn finish(&mut self) -> Result<Vec<TextToolCall>, String> {
        let held = std::mem::take(&mut self.held);
        if held.trim().is_empty() {
            return Err(held);
        }
        match parse_calls(&held) {
            Some(calls) if calls.iter().all(|c| self.offered.contains(&c.name)) => Ok(calls),
            _ => Err(held),
        }
    }
}

/// Parses text that is nothing but tool calls: JSON objects (optionally in
/// an array, `<tool_call>` tags or a ``` fence), each `{"name", "arguments"}`
/// (`"parameters"` is accepted too — some models use it).
fn parse_calls(text: &str) -> Option<Vec<TextToolCall>> {
    let mut body = text.trim();
    if let Some(rest) = body.strip_prefix("```") {
        // Drop the language tag line, then the closing fence.
        let rest = rest.split_once('\n').map(|(_, r)| r)?;
        body = rest.trim_end().strip_suffix("```")?.trim();
    }
    let body = body
        .replace("<tool_call>", "\n")
        .replace("</tool_call>", "\n");

    let mut calls = Vec::new();
    for value in serde_json::Deserializer::from_str(&body).into_iter::<Value>() {
        match value.ok()? {
            Value::Array(items) => {
                for item in items {
                    calls.push(to_call(item)?);
                }
            }
            item => calls.push(to_call(item)?),
        }
    }
    (!calls.is_empty()).then_some(calls)
}

fn to_call(value: Value) -> Option<TextToolCall> {
    let Value::Object(mut map) = value else {
        return None;
    };
    let name = map.remove("name")?.as_str()?.to_string();
    let arguments = map
        .remove("arguments")
        .or_else(|| map.remove("parameters"))
        .unwrap_or_else(|| Value::Object(Default::default()));
    // Arguments sometimes arrive as a JSON-encoded string.
    let arguments = match arguments {
        Value::String(raw) => serde_json::from_str(&raw).ok()?,
        other => other,
    };
    // Anything but name + arguments means this is some other JSON.
    (map.is_empty() && arguments.is_object()).then_some(TextToolCall { name, arguments })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(offered: &[&str], deltas: &[&str]) -> (String, Result<Vec<TextToolCall>, String>) {
        let mut filter = TextToolCallFilter::new(offered.iter().map(|s| s.to_string()).collect());
        let shown: String = deltas.iter().map(|d| filter.push(d)).collect();
        (shown, filter.finish())
    }

    fn weather(city: &str) -> TextToolCall {
        TextToolCall {
            name: "get_weather".into(),
            arguments: json!({ "city": city }),
        }
    }

    #[test]
    fn bare_json_split_across_deltas_is_a_tool_call() {
        let (shown, calls) = run(
            &["get_weather"],
            &[
                "{\"name\": \"get_",
                "weather\", \"arguments\": {\"city\": \"Paris\"}}",
            ],
        );
        assert_eq!(shown, "");
        assert_eq!(calls, Ok(vec![weather("Paris")]));
    }

    #[test]
    fn tags_fences_arrays_and_several_calls_are_recognized() {
        let tagged = "<tool_call>\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Oslo\"}}\n</tool_call>";
        assert_eq!(
            run(&["get_weather"], &[tagged]).1,
            Ok(vec![weather("Oslo")])
        );

        let fenced =
            "```json\n{\"name\": \"get_weather\", \"parameters\": {\"city\": \"Rome\"}}\n```";
        assert_eq!(
            run(&["get_weather"], &[fenced]).1,
            Ok(vec![weather("Rome")])
        );

        let two = "{\"name\": \"get_weather\", \"arguments\": {\"city\": \"A\"}}\n{\"name\": \"get_weather\", \"arguments\": \"{\\\"city\\\": \\\"B\\\"}\"}";
        assert_eq!(
            run(&["get_weather"], &[two]).1,
            Ok(vec![weather("A"), weather("B")])
        );

        let array = "[{\"name\": \"get_weather\", \"arguments\": {\"city\": \"C\"}}]";
        assert_eq!(run(&["get_weather"], &[array]).1, Ok(vec![weather("C")]));
    }

    #[test]
    fn prose_streams_through_immediately() {
        let (shown, rest) = run(&["get_weather"], &["  It", " is sunny {\"name\": 1}"]);
        assert_eq!(shown, "  It is sunny {\"name\": 1}");
        assert_eq!(rest, Err(String::new()));
    }

    #[test]
    fn json_that_is_not_a_call_to_an_offered_tool_comes_back_as_text() {
        for text in [
            "{\"name\": \"rm_rf\", \"arguments\": {}}", // not offered
            "{\"name\": \"get_weather\", \"city\": \"X\"}", // extra field
            "{\"answer\": 42}",                         // other JSON
            "{\"name\": \"get_weather\", \"arguments\": {\"c", // truncated
        ] {
            let (shown, rest) = run(&["get_weather"], &[text]);
            assert_eq!(shown, "");
            assert_eq!(rest, Err(text.to_string()), "{text}");
        }
    }

    #[test]
    fn without_offered_tools_nothing_is_held() {
        let text = "{\"name\": \"get_weather\", \"arguments\": {}}";
        let (shown, rest) = run(&[], &[text]);
        assert_eq!(shown, text);
        assert_eq!(rest, Err(String::new()));
    }
}
