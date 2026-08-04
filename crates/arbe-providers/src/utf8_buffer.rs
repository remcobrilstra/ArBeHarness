/// Buffers raw network-stream bytes across chunk boundaries and only
/// decodes the complete UTF-8 sequences currently available.
///
/// `reqwest::bytes_stream()` chunks are arbitrary byte-stream slices with no
/// respect for character boundaries — a multi-byte UTF-8 character (emoji,
/// accented text, CJK) can be split across two chunks. Decoding each chunk
/// independently via `String::from_utf8_lossy` would permanently replace the
/// split character with `�`; this buffer instead holds back any trailing
/// incomplete sequence until the bytes that complete it arrive.
#[derive(Debug, Default)]
pub struct Utf8ChunkBuffer {
    bytes: Vec<u8>,
}

impl Utf8ChunkBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `chunk` and returns the longest valid UTF-8 prefix now
    /// available, retaining any trailing incomplete sequence internally for
    /// the next call.
    pub fn push(&mut self, chunk: &[u8]) -> String {
        self.bytes.extend_from_slice(chunk);
        let valid_up_to = match std::str::from_utf8(&self.bytes) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        let decoded = String::from_utf8(self.bytes[..valid_up_to].to_vec())
            .expect("valid_up_to guarantees a valid UTF-8 prefix");
        self.bytes.drain(..valid_up_to);
        decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_ascii_unchanged() {
        let mut buf = Utf8ChunkBuffer::new();
        assert_eq!(buf.push(b"hello "), "hello ");
        assert_eq!(buf.push(b"world"), "world");
    }

    #[test]
    fn reassembles_a_multi_byte_character_split_across_chunks() {
        // "é" is 2 bytes in UTF-8: [0xC3, 0xA9].
        let full = "café".as_bytes().to_vec();
        let (first, second) = full.split_at(full.len() - 1);

        let mut buf = Utf8ChunkBuffer::new();
        let out1 = buf.push(first);
        assert_eq!(out1, "caf");
        let out2 = buf.push(second);
        assert_eq!(out2, "é");
        assert_eq!(format!("{out1}{out2}"), "café");
    }

    #[test]
    fn reassembles_a_three_byte_character_split_across_chunks() {
        // "€" is 3 bytes: [0xE2, 0x82, 0xAC]. Split after the first byte.
        let full = "€".as_bytes().to_vec();
        let mut buf = Utf8ChunkBuffer::new();
        assert_eq!(buf.push(&full[..1]), "");
        assert_eq!(buf.push(&full[1..2]), "");
        assert_eq!(buf.push(&full[2..]), "€");
    }
}
