//! Newline-delimited JSON, framed.
//!
//! Ollama's own api streams this rather than server-sent events: one complete
//! JSON document per line, no `data:` prefix, no blank-line dispatch. Simpler
//! than SSE, and different enough that treating one as the other yields a
//! stream that parses nothing at all.
//!
//! Frames come out shaped like SSE frames with no event name, so a parser does
//! not have to care which framing it was given.

use crate::sse::Frame;

#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..at + 1).take(at).collect();
            let text = String::from_utf8_lossy(&line).trim().to_string();
            if !text.is_empty() {
                out.push(Frame {
                    event: None,
                    data: text,
                });
            }
        }
        out
    }

    /// A last line with no newline after it, which is how a stream that was cut
    /// off mid-document looks as well as how a tidy one ends.
    pub fn finish(&mut self) -> Option<Frame> {
        let rest = std::mem::take(&mut self.buf);
        let text = String::from_utf8_lossy(&rest).trim().to_string();
        (!text.is_empty()).then_some(Frame {
            event: None,
            data: text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&str]) -> Vec<String> {
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(decoder.push(chunk.as_bytes()).into_iter().map(|f| f.data));
        }
        out.extend(decoder.finish().map(|f| f.data));
        out
    }

    #[test]
    fn each_line_is_a_document() {
        assert_eq!(
            all(&["{\"a\":1}\n{\"b\":2}\n"]),
            vec!["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn a_document_split_across_reads_is_reassembled() {
        assert_eq!(all(&["{\"a\":", "1}\n"]), vec!["{\"a\":1}"]);
    }

    #[test]
    fn a_last_line_without_a_newline_still_arrives() {
        assert_eq!(all(&["{\"a\":1}"]), vec!["{\"a\":1}"]);
    }

    #[test]
    fn blank_lines_are_not_documents() {
        assert_eq!(all(&["\n\n{\"a\":1}\n\n"]), vec!["{\"a\":1}"]);
    }

    #[test]
    fn carriage_returns_are_trimmed_off() {
        // Some proxies rewrite line endings.
        assert_eq!(all(&["{\"a\":1}\r\n"]), vec!["{\"a\":1}"]);
    }
}
