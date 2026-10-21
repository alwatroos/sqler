// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Incremental newline-delimited JSON line splitting.

use sqler_ai_api::AiError;

/// The longest NDJSON line accepted from the server: 8 MiB.
pub(crate) const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Splits a byte stream at `\n`. It splits bytes, not text, so a multibyte
/// UTF-8 sequence cut between two chunks is rejoined before it is decoded.
#[derive(Debug)]
pub(crate) struct LineDecoder {
    buf: Vec<u8>,
    limit: usize,
}

impl LineDecoder {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_LINE_BYTES)
    }

    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            limit,
        }
    }

    /// Adds a chunk and returns the complete, non-blank lines it finished.
    ///
    /// The second item is `AiError::Protocol` when a line is longer than the
    /// limit. The lines completed before that line are still returned.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> (Vec<Vec<u8>>, Option<AiError>) {
        // The bytes before `old_len` hold no newline, so the search starts there.
        let old_len = self.buf.len();
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut line_start = 0;
        let mut search_from = old_len;
        while let Some(i) = self.buf[search_from..].iter().position(|&b| b == b'\n') {
            let end = search_from + i;
            let line = &self.buf[line_start..end];
            if line.len() > self.limit {
                return (lines, Some(too_long()));
            }
            if let Some(l) = non_blank(line) {
                lines.push(l);
            }
            line_start = end + 1;
            search_from = line_start;
        }
        self.buf.drain(..line_start);
        if self.buf.len() > self.limit {
            return (lines, Some(too_long()));
        }
        (lines, None)
    }

    /// The last line when the stream ended without a final `\n`.
    pub(crate) fn finish(&mut self) -> Option<Vec<u8>> {
        let rest = std::mem::take(&mut self.buf);
        non_blank(&rest)
    }
}

fn non_blank(line: &[u8]) -> Option<Vec<u8>> {
    if line.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        Some(line.to_vec())
    }
}

fn too_long() -> AiError {
    AiError::Protocol("NDJSON line too long".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_lines(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut d = LineDecoder::new();
        let mut out = Vec::new();
        for c in chunks {
            let (lines, err) = d.push(c);
            assert!(err.is_none());
            out.extend(lines);
        }
        out.extend(d.finish());
        out
    }

    #[test]
    fn splits_at_every_byte_offset() {
        let data = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n";
        let want = vec![
            b"{\"a\":1}".to_vec(),
            b"{\"b\":2}".to_vec(),
            b"{\"c\":3}".to_vec(),
        ];
        for cut in 0..=data.len() {
            let (a, b) = data.split_at(cut);
            assert_eq!(all_lines(&[a, b]), want, "cut at {cut}");
        }
        let singles: Vec<&[u8]> = data.chunks(1).collect();
        assert_eq!(all_lines(&singles), want);
    }

    #[test]
    fn multibyte_utf8_split_across_chunks() {
        let data = "{\"t\":\"zażółć 日本\"}\n".as_bytes();
        for cut in 0..=data.len() {
            let (a, b) = data.split_at(cut);
            let lines = all_lines(&[a, b]);
            assert_eq!(lines.len(), 1);
            assert_eq!(
                std::str::from_utf8(&lines[0]).unwrap(),
                "{\"t\":\"zażółć 日本\"}"
            );
        }
    }

    #[test]
    fn skips_blank_lines_and_keeps_cr_for_the_json_parser() {
        let lines = all_lines(&[b"\n\r\n  \n{\"a\":1}\r\n\n"]);
        assert_eq!(lines, vec![b"{\"a\":1}\r".to_vec()]);
    }

    #[test]
    fn trailing_line_without_newline() {
        let mut d = LineDecoder::new();
        assert_eq!(d.push(b"{\"a\":1}").0.len(), 0);
        assert_eq!(d.finish(), Some(b"{\"a\":1}".to_vec()));
        assert_eq!(d.finish(), None);
    }

    #[test]
    fn oversize_line_is_a_protocol_error() {
        let mut d = LineDecoder::with_limit(8);
        assert_eq!(d.push(b"12345678").0.len(), 0);
        assert!(matches!(d.push(b"9").1, Some(AiError::Protocol(_))));
        let mut d = LineDecoder::with_limit(8);
        assert!(matches!(
            d.push(b"123456789\n").1,
            Some(AiError::Protocol(_))
        ));
        let mut d = LineDecoder::with_limit(8);
        let (lines, err) = d.push(b"12345678\n");
        assert_eq!(lines.len(), 1);
        assert!(err.is_none());
    }

    #[test]
    fn oversize_line_keeps_the_complete_lines_before_it() {
        let mut d = LineDecoder::with_limit(8);
        let (lines, err) = d.push(b"{a}\n{b}\n123456789\n");
        assert_eq!(lines, vec![b"{a}".to_vec(), b"{b}".to_vec()]);
        assert!(matches!(err, Some(AiError::Protocol(_))));
        let mut d = LineDecoder::with_limit(8);
        let (lines, err) = d.push(b"{a}\n123456789");
        assert_eq!(lines, vec![b"{a}".to_vec()]);
        assert!(matches!(err, Some(AiError::Protocol(_))));
    }
}
