//! Server-sent events, framed.
//!
//! `eventsource-stream` is not in the offline registry, so this is ours. It is
//! also eighty lines, which is the other reason.
//!
//! The rules that matter, from the WHATWG spec: lines end with `\n`, `\r\n` or
//! a bare `\r`; a blank line dispatches whatever has accumulated; a line
//! beginning with `:` is a comment; `field: value` loses exactly one space
//! after the colon; and repeated `data:` lines join with newlines between them.
//!
//! The subtlety is `\r\n` arriving split across two network reads. A decoder
//! that treats a trailing `\r` as a complete line terminator emits one spurious
//! blank line per chunk boundary, which dispatches half-built events. So a
//! trailing `\r` is held back until the next byte proves what it was.

/// One dispatched event.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// The `event:` field. OpenAI's Responses api uses it; Chat Completions
    /// does not send it at all.
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    data: String,
    event: Option<String>,
    started: bool,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Feed bytes, take whatever events they completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(line) = self.take_line() {
            if let Some(frame) = self.line(&line) {
                out.push(frame);
            }
        }
        out
    }

    /// End of stream. Dispatches a last event if the sender omitted the final
    /// blank line, which several gateways do.
    pub fn finish(&mut self) -> Option<Frame> {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&rest).to_string();
            if let Some(frame) = self.line(&line) {
                return Some(frame);
            }
        }
        self.dispatch()
    }

    /// One complete line, minus its terminator, or `None` if more bytes are
    /// needed to know where the line ends.
    fn take_line(&mut self) -> Option<String> {
        let pos = self.buf.iter().position(|b| *b == b'\n' || *b == b'\r')?;
        // A `\r` at the very end might be the first half of a `\r\n`.
        if self.buf[pos] == b'\r' && pos + 1 == self.buf.len() {
            return None;
        }
        let mut skip = 1;
        if self.buf[pos] == b'\r' && self.buf.get(pos + 1) == Some(&b'\n') {
            skip = 2;
        }
        let line: Vec<u8> = self.buf.drain(..pos + skip).take(pos).collect();
        let mut s = String::from_utf8_lossy(&line).to_string();
        if !self.started {
            self.started = true;
            // A leading byte-order mark is stripped once, per spec.
            if let Some(stripped) = s.strip_prefix('\u{feff}') {
                s = stripped.to_string();
            }
        }
        Some(s)
    }

    fn line(&mut self, line: &str) -> Option<Frame> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            "event" => self.event = Some(value.to_string()),
            // `id` and `retry` exist and neither provider uses them for
            // anything we act on.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<Frame> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        Some(Frame {
            event: self.event.take(),
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&str]) -> Vec<Frame> {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(d.push(c.as_bytes()));
        }
        out.extend(d.finish());
        out
    }

    #[test]
    fn a_simple_event_comes_out_whole() {
        let f = all(&["data: {\"a\":1}\n\n"]);
        assert_eq!(
            f,
            vec![Frame {
                event: None,
                data: "{\"a\":1}".into()
            }]
        );
    }

    #[test]
    fn an_event_name_is_kept() {
        let f = all(&["event: response.created\ndata: {}\n\n"]);
        assert_eq!(f[0].event.as_deref(), Some("response.created"));
    }

    #[test]
    fn a_crlf_split_across_reads_does_not_dispatch_twice() {
        // The bug this test exists for: `\r` at the end of one read looks like
        // a line terminator, and the following `\n` then looks like a blank
        // line, dispatching an event that was never finished.
        let f = all(&["data: one\r", "\ndata: two\r\n\r\n"]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].data, "one\ntwo");
    }

    #[test]
    fn one_event_arriving_in_many_pieces_is_reassembled() {
        let f = all(&["da", "ta: he", "llo\n", "\n"]);
        assert_eq!(f, vec![Frame { event: None, data: "hello".into() }]);
    }

    #[test]
    fn repeated_data_lines_join_with_newlines() {
        let f = all(&["data: a\ndata: b\ndata: c\n\n"]);
        assert_eq!(f[0].data, "a\nb\nc");
    }

    #[test]
    fn comments_and_unknown_fields_are_ignored() {
        let f = all(&[": keep-alive\nid: 7\nretry: 500\ndata: x\n\n"]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].data, "x");
    }

    #[test]
    fn a_missing_final_blank_line_still_dispatches() {
        let f = all(&["data: [DONE]"]);
        assert_eq!(f[0].data, "[DONE]");
    }

    #[test]
    fn only_one_space_after_the_colon_is_eaten() {
        let f = all(&["data:  leading\n\n"]);
        assert_eq!(f[0].data, " leading");
    }

    #[test]
    fn a_byte_order_mark_is_stripped_once() {
        let f = all(&["\u{feff}data: x\n\n"]);
        assert_eq!(f[0].data, "x");
    }

    #[test]
    fn a_blank_line_with_nothing_pending_dispatches_nothing() {
        assert!(all(&["\n\n\n"]).is_empty());
    }
}
