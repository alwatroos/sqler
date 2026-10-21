// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Incremental Server-Sent Events decoding.

use sqler_ai_api::AiError;

/// The longest line or event accepted from the server: 8 MiB.
pub(crate) const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// One decoded event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `event:` field, when the server sent one.
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// Splits a byte stream into SSE events. It splits bytes, not text, so a
/// multibyte UTF-8 sequence cut between two chunks is rejoined before it is
/// decoded.
#[derive(Debug)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
    event: Option<String>,
    limit: usize,
}

impl SseDecoder {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_EVENT_BYTES)
    }

    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            data: Vec::new(),
            has_data: false,
            event: None,
            limit,
        }
    }

    /// Adds a chunk and returns the events it completed.
    ///
    /// The second item is `AiError::Protocol` when a line or an event is longer
    /// than the limit. The events completed before that point are still returned.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> (Vec<SseEvent>, Option<AiError>) {
        // The bytes before `old_len` hold no newline, so the search starts there.
        let old_len = self.buf.len();
        self.buf.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut line_start = 0;
        let mut search_from = old_len;
        while let Some(i) = self.buf[search_from..].iter().position(|&b| b == b'\n') {
            let end = search_from + i;
            if end - line_start > self.limit {
                return (events, Some(too_long()));
            }
            let mut line = self.buf[line_start..end].to_vec();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            line_start = end + 1;
            search_from = line_start;
            if line.is_empty() {
                events.extend(self.take_event());
            } else if let Err(e) = self.field(&line) {
                self.buf.drain(..line_start);
                return (events, Some(e));
            }
        }
        self.buf.drain(..line_start);
        if self.buf.len() > self.limit {
            return (events, Some(too_long()));
        }
        (events, None)
    }

    /// The last event when the stream ended without a closing blank line.
    pub(crate) fn finish(&mut self) -> Option<SseEvent> {
        let mut rest = std::mem::take(&mut self.buf);
        if rest.last() == Some(&b'\r') {
            rest.pop();
        }
        if !rest.is_empty() {
            // An oversize tail cannot reach here: `push` already refused it.
            let _ = self.field(&rest);
        }
        self.take_event()
    }

    fn field(&mut self, line: &[u8]) -> Result<(), AiError> {
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (line, &line[line.len()..]),
        };
        match name {
            b"data" => {
                if self.data.len() + value.len() + 1 > self.limit {
                    return Err(too_long());
                }
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.has_data = true;
                self.data.extend_from_slice(value);
            }
            b"event" => self.event = Some(String::from_utf8_lossy(value).into_owned()),
            // `id`, `retry` and unknown fields are ignored.
            _ => {}
        }
        Ok(())
    }

    fn take_event(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        let has_data = std::mem::take(&mut self.has_data);
        let data = std::mem::take(&mut self.data);
        // An event without `data` carries nothing to dispatch.
        has_data.then(|| SseEvent {
            event,
            data: String::from_utf8_lossy(&data).into_owned(),
        })
    }
}

fn too_long() -> AiError {
    AiError::Protocol("SSE event too long".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_events(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            let (events, err) = d.push(c);
            assert!(err.is_none());
            out.extend(events);
        }
        out.extend(d.finish());
        out
    }

    fn ev(data: &str) -> SseEvent {
        SseEvent {
            event: None,
            data: data.to_owned(),
        }
    }

    #[test]
    fn splits_at_every_byte_offset() {
        let data = b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n";
        let want = vec![ev("{\"a\":1}"), ev("{\"b\":2}"), ev("[DONE]")];
        for cut in 0..=data.len() {
            let (a, b) = data.split_at(cut);
            assert_eq!(all_events(&[a, b]), want, "cut at {cut}");
        }
        let singles: Vec<&[u8]> = data.chunks(1).collect();
        assert_eq!(all_events(&singles), want);
    }

    #[test]
    fn multibyte_utf8_split_across_chunks() {
        let data = "data: {\"t\":\"zażółć 日本\"}\n\n".as_bytes();
        for cut in 0..=data.len() {
            let (a, b) = data.split_at(cut);
            assert_eq!(all_events(&[a, b]), vec![ev("{\"t\":\"zażółć 日本\"}")]);
        }
    }

    #[test]
    fn crlf_line_endings() {
        let got = all_events(&[b"data: a\r\n\r\ndata: b\r\n\r\n"]);
        assert_eq!(got, vec![ev("a"), ev("b")]);
    }

    #[test]
    fn multi_line_data_is_joined_with_newline() {
        let got = all_events(&[b"data: one\ndata:two\ndata:  three\n\n"]);
        assert_eq!(got, vec![ev("one\ntwo\n three")]);
    }

    #[test]
    fn comments_ids_and_retry_are_ignored() {
        let got = all_events(&[b": OPENROUTER PROCESSING\n\nid: 7\nretry: 100\n: c\ndata: x\n\n"]);
        assert_eq!(got, vec![ev("x")]);
    }

    #[test]
    fn event_field_is_recorded() {
        let got = all_events(&[b"event: error\ndata: {\"error\":{}}\n\ndata: y\n\n"]);
        assert_eq!(
            got,
            vec![
                SseEvent {
                    event: Some("error".into()),
                    data: "{\"error\":{}}".into()
                },
                ev("y")
            ]
        );
    }

    #[test]
    fn trailing_event_without_blank_line() {
        let mut d = SseDecoder::new();
        assert_eq!(d.push(b"data: [DONE]").0.len(), 0);
        assert_eq!(d.finish(), Some(ev("[DONE]")));
        assert_eq!(d.finish(), None);
        let mut d = SseDecoder::new();
        assert_eq!(d.push(b"data: a\ndata: b\r").0.len(), 0);
        assert_eq!(d.finish(), Some(ev("a\nb")));
    }

    #[test]
    fn oversize_line_is_a_protocol_error() {
        let mut d = SseDecoder::with_limit(16);
        assert_eq!(d.push(b"data: 123456789").0.len(), 0);
        assert!(matches!(d.push(b"01").1, Some(AiError::Protocol(_))));
        let mut d = SseDecoder::with_limit(16);
        assert!(matches!(
            d.push(b"data: 12345678901\n").1,
            Some(AiError::Protocol(_))
        ));
    }

    #[test]
    fn oversize_event_is_a_protocol_error_and_keeps_earlier_events() {
        let mut d = SseDecoder::with_limit(16);
        let (events, err) = d.push(b"data: ok\n\ndata: 1234567890\ndata: 1234567890\n\n");
        assert_eq!(events, vec![ev("ok")]);
        assert!(matches!(err, Some(AiError::Protocol(_))));
    }
}
