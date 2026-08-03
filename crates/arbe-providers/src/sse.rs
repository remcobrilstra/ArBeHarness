/// Incrementally decodes an OpenAI-style `text/event-stream` body into
/// `data:` payloads, buffering partial lines across chunk boundaries (a
/// chunk boundary can land mid-line since it's just a byte stream).
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: String,
}

/// One decoded SSE data payload, or the `[DONE]` sentinel OpenAI sends to
/// close the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseItem {
    Data(String),
    Done,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk of raw bytes and returns any complete `data:` lines
    /// found so far. Call this once per chunk read from the response body.
    pub fn push(&mut self, chunk: &str) -> Vec<SseItem> {
        self.buffer.push_str(chunk);
        let mut items = Vec::new();

        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos]
                .trim_end_matches('\r')
                .to_string();
            self.buffer.drain(..=newline_pos);

            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if payload == "[DONE]" {
                items.push(SseItem::Done);
            } else {
                items.push(SseItem::Data(payload.to_string()));
            }
        }

        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_single_complete_chunk() {
        let mut decoder = SseDecoder::new();
        let items = decoder.push("data: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(
            items,
            vec![SseItem::Data("{\"a\":1}".to_string()), SseItem::Done,]
        );
    }

    #[test]
    fn reassembles_a_line_split_across_two_chunks() {
        let mut decoder = SseDecoder::new();
        assert_eq!(decoder.push("data: {\"a\""), Vec::new());
        let items = decoder.push(":1}\n");
        assert_eq!(items, vec![SseItem::Data("{\"a\":1}".to_string())]);
    }

    #[test]
    fn ignores_blank_lines_and_comments() {
        let mut decoder = SseDecoder::new();
        let items = decoder.push(": keep-alive\n\ndata: {\"a\":1}\n\n");
        assert_eq!(items, vec![SseItem::Data("{\"a\":1}".to_string())]);
    }
}
