// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Incremental server-sent-events decoding.
//!
//! The decoder works on bytes, so a multibyte UTF-8 sequence cut between two
//! chunks is rejoined before the event text is decoded.

use sqler_ai_api::AiError;

/// The largest event accepted from the server: 8 MiB.
pub(crate) const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// A UTF-8 byte order mark, dropped at the start of the stream.
const BOM: [u8; 3] = [0xef, 0xbb, 0xbf];

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `event:` field, if any. The mapper ignores it and reads the `type`
    /// of the JSON data instead.
    // Kept for the decoder contract and its tests; the mapper does not read it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub event: Option<String>,
    /// The `data:` lines joined with `\n`. Never empty.
    pub data: String,
}

/// Splits a byte stream into events. Lines end at `\n`, `\r\n` or `\r`.
#[derive(Debug)]
pub(crate) struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    event: Vec<u8>,
    has_data: bool,
    /// The previous byte was `\r`, so a following `\n` belongs to it.
    after_cr: bool,
    /// Bytes of the event being built (lines, with their terminators).
    size: usize,
    limit: usize,
    /// How many bytes of a leading UTF-8 BOM have been matched so far; 3
    /// once the start of the stream is past.
    bom: usize,
}

impl SseDecoder {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_EVENT_BYTES)
    }

    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            line: Vec::new(),
            data: Vec::new(),
            event: Vec::new(),
            has_data: false,
            after_cr: false,
            size: 0,
            limit,
            bom: 0,
        }
    }

    /// Adds a chunk and returns the events it completed.
    ///
    /// The second item is `AiError::Protocol` when an event is larger than the
    /// limit. The events completed before that point are still returned.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> (Vec<SseEvent>, Option<AiError>) {
        let mut events = Vec::new();
        for &b in chunk {
            if self.bom < BOM.len() {
                if b == BOM[self.bom] {
                    self.bom += 1;
                    continue;
                }
                // Not a BOM after all: the bytes held back are line content.
                self.line.extend_from_slice(&BOM[..self.bom]);
                self.bom = BOM.len();
            }
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    if let Some(ev) = self.end_line() {
                        events.push(ev);
                    }
                }
                _ => {
                    self.line.push(b);
                    if self.size + self.line.len() > self.limit {
                        return (events, Some(too_large()));
                    }
                }
            }
        }
        (events, None)
    }

    /// The event still being built when the stream ended, if its data is not
    /// empty. A server that closes right after the last data line, without
    /// the blank line, still delivers that event.
    pub(crate) fn finish(&mut self) -> Option<SseEvent> {
        if self.bom < BOM.len() {
            self.line.extend_from_slice(&BOM[..self.bom]);
            self.bom = BOM.len();
        }
        let ev = if self.line.is_empty() {
            None
        } else {
            self.end_line()
        };
        ev.or_else(|| self.dispatch())
    }

    fn end_line(&mut self) -> Option<SseEvent> {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return self.dispatch();
        }
        self.size += line.len() + 1;
        if line[0] == b':' {
            return None;
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (&line[..], &[][..]),
        };
        match name {
            b"data" => {
                if self.has_data {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                self.has_data = true;
            }
            b"event" => {
                self.event.clear();
                self.event.extend_from_slice(value);
            }
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let data = std::mem::take(&mut self.data);
        let event = std::mem::take(&mut self.event);
        self.has_data = false;
        self.size = 0;
        if data.is_empty() {
            return None;
        }
        Some(SseEvent {
            event: (!event.is_empty()).then(|| String::from_utf8_lossy(&event).into_owned()),
            data: String::from_utf8_lossy(&data).into_owned(),
        })
    }
}

fn too_large() -> AiError {
    AiError::Protocol("SSE event too large".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            let (evs, err) = d.push(c);
            assert!(err.is_none());
            out.extend(evs);
        }
        out.extend(d.finish());
        out
    }

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    const SAMPLE: &[u8] =
        b": comment\nevent: a\ndata: one\n\nevent: b\ndata: x\ndata: y\n\ndata:no-space\n\n";

    fn expected() -> Vec<SseEvent> {
        vec![
            ev(Some("a"), "one"),
            ev(Some("b"), "x\ny"),
            ev(None, "no-space"),
        ]
    }

    #[test]
    fn whole_input() {
        assert_eq!(decode_all(&[SAMPLE]), expected());
    }

    #[test]
    fn split_at_every_byte_offset() {
        for cut in 0..=SAMPLE.len() {
            let (a, b) = SAMPLE.split_at(cut);
            assert_eq!(decode_all(&[a, b]), expected(), "cut {cut}");
        }
        let singles: Vec<&[u8]> = SAMPLE.chunks(1).collect();
        assert_eq!(decode_all(&singles), expected());
    }

    #[test]
    fn crlf_cr_and_cr_then_lf_across_chunks() {
        let want = vec![ev(None, "a"), ev(None, "b")];
        assert_eq!(decode_all(&[b"data: a\r\n\r\ndata: b\r\n\r\n"]), want);
        assert_eq!(decode_all(&[b"data: a\r\rdata: b\r\r"]), want);
        // A `\r` at the end of one chunk and `\n` at the start of the next is
        // one line break, not two (which would dispatch early).
        assert_eq!(
            decode_all(&[b"data: a\r", b"\n\r", b"\ndata: b\r\n\r\n"]),
            want
        );
        assert_eq!(
            decode_all(&[b"data: a\r", b"\n", b"\r\n", b"data: b\n\n"]),
            want
        );
    }

    #[test]
    fn empty_data_and_unknown_fields_are_skipped() {
        let got = decode_all(&[b"event: ping\n\nid: 7\nretry: 5\n\ndata:\n\ndata: z\n\n"]);
        assert_eq!(got, vec![ev(None, "z")]);
    }

    #[test]
    fn only_one_space_is_stripped() {
        assert_eq!(decode_all(&[b"data:  two\n\n"]), vec![ev(None, " two")]);
    }

    #[test]
    fn multibyte_utf8_cut_between_chunks() {
        let bytes = "data: h\u{e9}llo \u{1f980}\n\n".as_bytes();
        for cut in 0..=bytes.len() {
            let (a, b) = bytes.split_at(cut);
            assert_eq!(
                decode_all(&[a, b]),
                vec![ev(None, "h\u{e9}llo \u{1f980}")],
                "cut {cut}"
            );
        }
    }

    #[test]
    fn last_event_without_blank_line_is_delivered() {
        assert_eq!(
            decode_all(&[b"data: a\n\ndata: tail"]),
            vec![ev(None, "a"), ev(None, "tail")]
        );
        assert_eq!(decode_all(&[b"data: a\n"]), vec![ev(None, "a")]);
        assert_eq!(decode_all(&[b"event: x\n"]), vec![]);
    }

    #[test]
    fn oversize_event_is_a_protocol_error() {
        let mut d = SseDecoder::with_limit(32);
        let (evs, err) = d.push(b"data: ok\n\n");
        assert_eq!(evs.len(), 1);
        assert!(err.is_none());
        let (evs, err) = d.push(&[b'x'; 64]);
        assert_eq!(evs, vec![]);
        assert!(matches!(err, Some(AiError::Protocol(_))));

        // Many small lines of one event add up too.
        let mut d = SseDecoder::with_limit(32);
        let mut failed = false;
        for _ in 0..20 {
            if d.push(b"data: abc\n").1.is_some() {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }

    #[test]
    fn events_before_the_oversize_point_are_returned() {
        let mut d = SseDecoder::with_limit(16);
        let mut input = b"data: a\n\n".to_vec();
        input.extend_from_slice(&[b'y'; 40]);
        let (evs, err) = d.push(&input);
        assert_eq!(evs, vec![ev(None, "a")]);
        assert!(err.is_some());
    }

    /// Recorded streams cut at every byte offset (two chunks) and byte by
    /// byte, with LF, CRLF and CR terminators, give the same events.
    #[test]
    fn recorded_fixtures_split_at_every_offset_and_terminator() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/");
        for name in ["text.sse", "tool.sse", "final.sse", "error_event.sse"] {
            let raw = std::fs::read(format!("{dir}{name}")).unwrap();
            let lf: Vec<u8> = String::from_utf8(raw)
                .unwrap()
                .replace("\r\n", "\n")
                .into_bytes();
            let want = decode_all(&[&lf]);
            assert!(!want.is_empty(), "{name}");
            let crlf = String::from_utf8(lf.clone())
                .unwrap()
                .replace('\n', "\r\n")
                .into_bytes();
            let cr = String::from_utf8(lf.clone())
                .unwrap()
                .replace('\n', "\r")
                .into_bytes();
            for (label, bytes) in [("lf", &lf), ("crlf", &crlf), ("cr", &cr)] {
                for cut in 0..=bytes.len() {
                    let (a, b) = bytes.split_at(cut);
                    assert_eq!(decode_all(&[a, b]), want, "{name} {label} cut {cut}");
                }
                let singles: Vec<&[u8]> = bytes.chunks(1).collect();
                assert_eq!(decode_all(&singles), want, "{name} {label} bytes");
            }
        }
    }

    /// Deterministic pseudo-random chunking of a mixed-terminator stream.
    #[test]
    fn random_chunking_matches_whole_input() {
        let input: &[u8] = b": c\r\nevent: a\r\ndata: one\r\ndata: two\r\n\r\n: x\ndata: \xe2\x82\xac\n\ndata: z\r\r";
        let want = decode_all(&[input]);
        assert_eq!(want.len(), 3);
        assert_eq!(want[0], ev(Some("a"), "one\ntwo"));
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..2000 {
            let mut chunks: Vec<&[u8]> = Vec::new();
            let mut rest = input;
            while !rest.is_empty() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let n = 1 + (seed % 6) as usize;
                let (a, b) = rest.split_at(n.min(rest.len()));
                chunks.push(a);
                rest = b;
            }
            assert_eq!(decode_all(&chunks), want);
        }
    }

    #[test]
    fn leading_bom_is_ignored() {
        assert_eq!(
            decode_all(&[b"\xef\xbb\xbfdata: first\n\ndata: second\n\n"]),
            vec![ev(None, "first"), ev(None, "second")]
        );
        // Split inside the BOM.
        assert_eq!(
            decode_all(&[b"\xef", b"\xbb", b"\xbfdata: a\n\n"]),
            vec![ev(None, "a")]
        );
        // Only the very first bytes count.
        assert_eq!(
            decode_all(&[b"data: a\n\n\xef\xbb\xbfdata: b\n\n"]),
            vec![ev(None, "a")]
        );
    }
}
