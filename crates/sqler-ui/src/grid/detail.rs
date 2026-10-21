// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The cell detail: one cell, in full, as lines a view can read by index.
//!
//! [`CellDetail`] is headless. It keeps the (bounded) text or bytes of one cell and a line index;
//! a view asks for the lines it draws with [`CellDetail::line`], so a 1 MiB value is never turned
//! into 1 MiB of elements. Text is shown as stored, JSON is re-indented without changing key
//! order, numbers or escapes, and binary is a hex dump built row by row. Nothing in a value is
//! interpreted.
//!
//! Values may hold secrets: no type here prints a value in `Debug`, and nothing is logged.

use std::borrow::Cow;
use std::fmt;

use sqler_driver_api::sqler_core::Value;

use super::cell::{ColumnHeader, group_thousands};
use super::store::CellRef;

/// Bytes of text, raw JSON or pretty-printed JSON shown while bounded.
pub const DETAIL_TEXT_CAP: usize = 1 << 20;
/// Bytes of binary dumped as hex while bounded.
pub const DETAIL_BINARY_CAP: usize = 1 << 20;
/// Larger raw JSON is not formatted while bounded.
pub const JSON_PRETTY_CAP: usize = 1 << 20;
/// Raw JSON larger than this is never formatted, not even by Show all: re-indenting a huge
/// document costs many times its size in memory. It is shown as stored.
pub const JSON_FORMAT_MAX: usize = 8 << 20;
/// Formatted JSON is cut at this many bytes even by Show all (deep documents grow when
/// indented).
const JSON_PRETTY_MAX: usize = 64 << 20;
/// A longer line is split into continued segments of this many characters.
pub const DETAIL_LINE_CHARS: usize = 1_000;
/// Bytes per row of the hex dump.
pub const HEX_BYTES_PER_LINE: usize = 16;

/// Line offsets are `u32`, so even an unbounded detail shows at most this many bytes of text.
const INDEX_LIMIT: usize = u32::MAX as usize;
/// Columns per tab stop.
const TAB_WIDTH: usize = 4;

/// How much of a value a [`CellDetail`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailCap {
    /// At most the `DETAIL_*` caps.
    Bounded,
    /// Everything (this can be large: build it off the UI thread).
    Full,
}

/// What a [`CellDetail`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DetailKind {
    /// SQL `NULL`.
    Null,
    /// A number, boolean, date or time: one line, as the grid shows it.
    Scalar,
    /// Text, as stored.
    Text,
    /// Valid JSON, re-indented.
    Json,
    /// JSON shown as stored: not valid, too deep or too large to format.
    RawJson,
    /// Binary, as a hex dump.
    Binary,
}

/// The shown part of a value.
enum Body {
    None,
    /// Text and the byte offset at which each line (segment) starts.
    Text {
        text: String,
        starts: Vec<u32>,
        /// Index of the line with the most display columns (the first one on a tie).
        widest: usize,
    },
    /// Bytes, dumped row by row.
    Hex(Vec<u8>),
}

/// A borrowed cell, the common shape of [`CellRef`] and [`Value`].
enum Src<'a> {
    Null,
    Text(&'a str),
    Json(&'a str),
    Bytes(&'a [u8]),
    Scalar(String),
}

impl<'a> Src<'a> {
    fn from_cell(c: CellRef<'a>) -> Self {
        match c {
            CellRef::Null => Self::Null,
            CellRef::Text(s) => Self::Text(s),
            CellRef::Json(s) => Self::Json(s),
            CellRef::Bytes(b) => Self::Bytes(b),
            CellRef::Value(v) => Self::from_value(v),
            other => Self::Scalar(other.to_string()),
        }
    }

    fn from_value(v: &'a Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Text(s) => Self::Text(s),
            Value::Json(s) => Self::Json(s),
            Value::Bytes(b) => Self::Bytes(b),
            other => Self::Scalar(other.to_string()),
        }
    }
}

/// One cell, formatted for the detail pane.
pub struct CellDetail {
    column: String,
    type_label: &'static str,
    kind: DetailKind,
    cap: DetailCap,
    total: usize,
    shown: usize,
    truncated: bool,
    notes: Vec<String>,
    body: Body,
}

/// Prints the shape of the detail, never the value (it may be a secret).
impl fmt::Debug for CellDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CellDetail")
            .field("kind", &self.kind)
            .field("cap", &self.cap)
            .field("total_bytes", &self.total)
            .field("shown_bytes", &self.shown)
            .field("lines", &self.line_count())
            .finish_non_exhaustive()
    }
}

impl CellDetail {
    /// Formats a cell of the model.
    #[must_use]
    pub fn build(h: &ColumnHeader, cell: CellRef<'_>, cap: DetailCap) -> Self {
        Self::from_src(h, Src::from_cell(cell), cap)
    }

    /// Formats an owned value: the same result as [`CellDetail::build`] for the same cell.
    #[must_use]
    pub fn build_owned(h: &ColumnHeader, v: &Value, cap: DetailCap) -> Self {
        Self::from_src(h, Src::from_value(v), cap)
    }

    fn from_src(h: &ColumnHeader, src: Src<'_>, cap: DetailCap) -> Self {
        let mut d = Self {
            column: h.name.clone(),
            type_label: h.type_label,
            kind: DetailKind::Null,
            cap,
            total: 0,
            shown: 0,
            truncated: false,
            notes: Vec::new(),
            body: Body::None,
        };
        let text_cap = match cap {
            DetailCap::Bounded => DETAIL_TEXT_CAP,
            DetailCap::Full => INDEX_LIMIT,
        };
        match src {
            Src::Null => {}
            Src::Scalar(s) => {
                d.kind = DetailKind::Scalar;
                d.total = s.len();
                d.shown = s.len();
                d.set_text(s);
            }
            Src::Text(s) => {
                d.kind = DetailKind::Text;
                d.total = s.len();
                if s.is_empty() {
                    d.notes.push("Empty text (not NULL).".to_owned());
                }
                d.set_text_prefix(s, text_cap);
            }
            Src::Json(s) => d.set_json(s, cap, text_cap),
            Src::Bytes(b) => {
                d.kind = DetailKind::Binary;
                d.total = b.len();
                let keep = match cap {
                    DetailCap::Bounded => b.len().min(DETAIL_BINARY_CAP),
                    DetailCap::Full => b.len(),
                };
                d.shown = keep;
                d.truncated = keep < b.len();
                d.push_cut_note();
                d.body = Body::Hex(b[..keep].to_vec());
            }
        }
        d
    }

    /// Keeps the first `limit` bytes of `s` (on a char boundary) as the body.
    fn set_text_prefix(&mut self, s: &str, limit: usize) {
        let keep = floor_boundary(s, limit);
        self.shown = keep;
        self.truncated = keep < s.len();
        self.push_cut_note();
        self.set_text(s[..keep].to_owned());
    }

    /// Stores the text and builds its line index.
    fn set_text(&mut self, text: String) {
        let (starts, widest) = index_lines(&text);
        self.body = Body::Text {
            text,
            starts,
            widest,
        };
    }

    /// The "Showing the first … of …" note, when the value was cut.
    fn push_cut_note(&mut self) {
        if self.truncated {
            self.notes.push(format!(
                "Showing the first {} of {}.",
                human_bytes(self.shown),
                human_bytes(self.total)
            ));
        }
    }

    fn set_json(&mut self, raw: &str, cap: DetailCap, text_cap: usize) {
        self.total = raw.len();
        let huge = raw.len() > JSON_FORMAT_MAX;
        let too_large = !huge && cap == DetailCap::Bounded && raw.len() > JSON_PRETTY_CAP;
        // Only a small enough document is checked (the check never leaves this function with the
        // input in an error message, and it builds no tree).
        if !huge && !too_large && is_valid_json(raw) {
            self.kind = DetailKind::Json;
            let limit = match cap {
                DetailCap::Bounded => text_cap,
                DetailCap::Full => JSON_PRETTY_MAX,
            };
            let (pretty, cut) = reindent(raw, limit);
            self.shown = pretty.len();
            self.truncated = cut && cap == DetailCap::Bounded;
            if cut {
                self.notes.push(format!(
                    "Showing the first {} of the formatted value.",
                    human_bytes(limit)
                ));
            }
            self.set_text(pretty);
            return;
        }
        self.kind = DetailKind::RawJson;
        self.set_text_prefix(raw, text_cap);
        self.notes.push(if huge {
            "Too large to format; shown as stored.".to_owned()
        } else if too_large {
            "Too large to format here; shown as stored. Use Show all to format it.".to_owned()
        } else {
            "Not formatted: not valid JSON, or nested at depth 128 or deeper. Shown as stored."
                .to_owned()
        });
    }

    /// The column name.
    #[must_use]
    pub fn column(&self) -> &str {
        &self.column
    }

    /// The type label of the column.
    #[must_use]
    pub fn type_label(&self) -> &'static str {
        self.type_label
    }

    /// What the detail shows.
    #[must_use]
    pub fn kind(&self) -> DetailKind {
        self.kind
    }

    /// How much of the value the detail keeps.
    #[must_use]
    pub fn cap(&self) -> DetailCap {
        self.cap
    }

    /// Size of the stored value in bytes.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.total
    }

    /// Bytes of the value that are shown.
    #[must_use]
    pub fn shown_bytes(&self) -> usize {
        self.shown
    }

    /// Whether [`DetailCap::Full`] would show more.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated && self.cap == DetailCap::Bounded
    }

    /// One line for the header: `NULL`, `text · 12,345 bytes`.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.kind == DetailKind::Null {
            return "NULL".to_owned();
        }
        format!(
            "{} · {} bytes",
            self.type_label,
            group_thousands(self.total)
        )
    }

    /// Notes to show above the body.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Number of lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        match &self.body {
            Body::None => 0,
            Body::Text { starts, .. } => starts.len(),
            Body::Hex(bytes) => bytes.len().div_ceil(HEX_BYTES_PER_LINE),
        }
    }

    /// Index of the line that is widest when drawn, for sizing the horizontal scroll range.
    ///
    /// `None` when there is no line.
    #[must_use]
    pub fn widest_line(&self) -> Option<usize> {
        match &self.body {
            Body::None => None,
            Body::Text { starts, widest, .. } => (!starts.is_empty()).then_some(*widest),
            // Every row but a last partial one has the same width.
            Body::Hex(bytes) => (!bytes.is_empty()).then_some(0),
        }
    }

    /// The line at `ix`, ready to draw.
    #[must_use]
    pub fn line(&self, ix: usize) -> Option<DetailLine<'_>> {
        match &self.body {
            Body::None => None,
            Body::Text { text, starts, .. } => {
                let start = *starts.get(ix)? as usize;
                let end = starts.get(ix + 1).map_or(text.len(), |e| *e as usize);
                let mut seg = &text[start..end];
                if let Some(s) = seg.strip_suffix('\n') {
                    seg = s.strip_suffix('\r').unwrap_or(s);
                }
                let continued = start > 0 && text.as_bytes()[start - 1] != b'\n';
                Some(DetailLine {
                    text: display_text(seg),
                    continued,
                })
            }
            Body::Hex(bytes) => {
                let off = ix.checked_mul(HEX_BYTES_PER_LINE)?;
                if off >= bytes.len() {
                    return None;
                }
                let row = bytes.get(off..)?;
                let row = &row[..row.len().min(HEX_BYTES_PER_LINE)];
                Some(DetailLine {
                    text: Cow::Owned(hex_row(off, row)),
                    continued: false,
                })
            }
        }
    }
}

/// One line of a [`CellDetail`].
pub struct DetailLine<'a> {
    /// The text to draw, with the display substitutions applied.
    pub text: Cow<'a, str>,
    /// Whether the line continues a longer one that was split.
    pub continued: bool,
}

/// Prints no text: a line may be a secret.
impl fmt::Debug for DetailLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DetailLine")
            .field("bytes", &self.text.len())
            .field("continued", &self.continued)
            .finish_non_exhaustive()
    }
}

/// The text a Copy puts on the clipboard: the full value as stored, or `None` for NULL.
///
/// JSON is the raw text (not re-indented), binary is `0x` and hex digits, other values are what
/// the grid shows.
#[must_use]
pub fn copy_text(cell: CellRef<'_>) -> Option<String> {
    match cell {
        CellRef::Null | CellRef::Value(Value::Null) => None,
        CellRef::Text(s) | CellRef::Json(s) => Some(s.to_owned()),
        CellRef::Bytes(b) => Some(hex_literal(b)),
        CellRef::Value(Value::Bytes(b)) => Some(hex_literal(b)),
        other => Some(other.to_string()),
    }
}

/// `0x` and the lowercase hex digits of `bytes`: the text `Display` gives, built with a lookup
/// table so that 50 MB does not take long.
fn hex_literal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = vec![0u8; 2 + 2 * bytes.len()];
    out[..2].copy_from_slice(b"0x");
    for (pair, b) in out[2..].as_chunks_mut::<2>().0.iter_mut().zip(bytes) {
        pair[0] = HEX[usize::from(b >> 4)];
        pair[1] = HEX[usize::from(b & 0xf)];
    }
    // Only ASCII was written.
    String::from_utf8(out).unwrap_or_default()
}

/// Nesting at this depth or deeper is never formatted (the indent would grow without bound).
const JSON_MAX_DEPTH: usize = 128;

/// Whether `raw` is one valid JSON document nested less than [`JSON_MAX_DEPTH`] levels deep.
///
/// Streams over it without building a tree, so it needs no memory to speak of, and accepts
/// numbers that do not fit an `f64` (`1e400`).
fn is_valid_json(raw: &str) -> bool {
    shallow(raw.as_bytes()) && serde_json::from_str::<serde::de::IgnoredAny>(raw).is_ok()
}

/// Whether no `{` or `[` outside a string opens at depth `JSON_MAX_DEPTH` or deeper.
fn shallow(b: &[u8]) -> bool {
    let (mut in_str, mut esc, mut depth) = (false, false, 0usize);
    for &c in b {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                if depth >= JSON_MAX_DEPTH {
                    return false;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

/// A byte count for people: `512 bytes`, `4.5 KiB`, `1.0 MiB`.
#[must_use]
pub fn human_bytes(n: usize) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} bytes");
    }
    #[allow(clippy::cast_precision_loss)] // one decimal of a size: a rounded f64 is exact enough
    let mut x = n as f64 / 1024.0;
    let mut unit = 0;
    while x >= 1023.95 && unit + 1 < UNITS.len() {
        x /= 1024.0;
        unit += 1;
    }
    format!("{x:.1} {}", UNITS[unit])
}

/// The largest index `<= limit` that is a char boundary of `s` (or `s.len()`).
fn floor_boundary(s: &str, limit: usize) -> usize {
    let mut n = limit.min(s.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    n
}

/// Byte offsets at which each line, and each continued segment of a long line, starts, and the
/// index of the segment with the most display columns.
///
/// A line ends at `\n`; a text that ends with `\n` has no empty last line, and an empty text has
/// no line at all. A line is cut into segments of at most [`DETAIL_LINE_CHARS`] display columns,
/// where a tab counts as the columns up to its tab stop (as drawn).
fn index_lines(text: &str) -> (Vec<u32>, usize) {
    let bytes = text.as_bytes();
    #[allow(clippy::naive_bytecount)] // one pass to size the index; no extra dependency
    let lines = bytes.iter().filter(|b| **b == b'\n').count() + 1;
    let mut starts = Vec::with_capacity(lines);
    // Offsets fit: the text is cut at `INDEX_LIMIT`.
    let offset = |p: usize| u32::try_from(p).unwrap_or(u32::MAX);
    let (mut widest, mut widest_cols) = (0usize, 0usize);
    let mut pos = 0;
    while pos < text.len() {
        let (content_end, next) = match text[pos..].find('\n') {
            Some(i) => {
                let nl = pos + i;
                let end = if nl > pos && bytes[nl - 1] == b'\r' {
                    nl - 1
                } else {
                    nl
                };
                (end, nl + 1)
            }
            None => (text.len(), text.len()),
        };
        starts.push(offset(pos));
        let mut cols = 0usize;
        for (off, c) in text[pos..content_end].char_indices() {
            let mut w = tab_cols(c, cols);
            if cols + w > DETAIL_LINE_CHARS {
                if cols > widest_cols {
                    (widest, widest_cols) = (starts.len() - 1, cols);
                }
                starts.push(offset(pos + off));
                cols = 0;
                w = tab_cols(c, 0);
            }
            cols += w;
        }
        if cols > widest_cols {
            (widest, widest_cols) = (starts.len() - 1, cols);
        }
        pos = next;
    }
    (starts, widest)
}

/// Display columns of `c` when drawn at column `col` of its segment.
fn tab_cols(c: char, col: usize) -> usize {
    if c == '\t' {
        TAB_WIDTH - col % TAB_WIDTH
    } else {
        1
    }
}

/// Whether the character is replaced when drawn: control characters and bidi controls.
fn needs_display_change(c: char) -> bool {
    (c as u32) < 0x20 || ('\u{7f}'..='\u{9f}').contains(&c) || is_bidi(c)
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Applies the display substitutions to one line: tabs to spaces, C0 controls and DEL to their
/// visible symbols, bidi and C1 controls to U+FFFD. The text itself is not changed anywhere else.
fn display_text(seg: &str) -> Cow<'_, str> {
    if !seg.chars().any(needs_display_change) {
        return Cow::Borrowed(seg);
    }
    let mut out = String::with_capacity(seg.len());
    let mut col = 0usize;
    for c in seg.chars() {
        match c {
            '\t' => {
                let n = tab_cols(c, col);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
                continue;
            }
            '\u{7f}' => out.push('\u{2421}'),
            c if is_bidi(c) || ('\u{80}'..='\u{9f}').contains(&c) => out.push('\u{fffd}'),
            c if (c as u32) < 0x20 => {
                out.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}'));
            }
            c => out.push(c),
        }
        col += 1;
    }
    Cow::Owned(out)
}

/// One row of the hex dump: offset, 16 hex bytes (a gap after the 8th), then `|ascii|`.
fn hex_row(offset: usize, row: &[u8]) -> String {
    let mut out = format!("{offset:08x}  ");
    for i in 0..HEX_BYTES_PER_LINE {
        if i > 0 {
            out.push(' ');
        }
        if i == HEX_BYTES_PER_LINE / 2 {
            out.push(' ');
        }
        match row.get(i) {
            Some(b) => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 0xf)]));
            }
            None => out.push_str("  "),
        }
    }
    out.push_str("  |");
    out.extend(row.iter().map(|b| {
        if (0x20..=0x7e).contains(b) {
            char::from(*b)
        } else {
            '.'
        }
    }));
    out.push('|');
    out
}

/// Re-indents valid JSON text with two spaces: key order, number text and string escapes are
/// kept. Returns the result, cut at `limit` bytes on a char boundary, and whether it was cut.
///
/// `raw` must be valid JSON (checked by the caller); on anything else the result is unspecified
/// but safe.
fn reindent(raw: &str, limit: usize) -> (String, bool) {
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len().min(limit).saturating_add(64));
    let (mut in_str, mut esc, mut depth) = (false, false, 0usize);
    let mut i = 0;
    let newline = |out: &mut Vec<u8>, depth: usize| {
        out.push(b'\n');
        out.extend(std::iter::repeat_n(b' ', depth * 2));
    };
    while i < b.len() && out.len() <= limit {
        let c = b[i];
        i += 1;
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push(c);
            }
            b' ' | b'\t' | b'\n' | b'\r' => {}
            b'{' | b'[' => {
                let close = if c == b'{' { b'}' } else { b']' };
                let mut j = i;
                while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n' | b'\r') {
                    j += 1;
                }
                if b.get(j) == Some(&close) {
                    out.push(c);
                    out.push(close);
                    i = j + 1;
                } else {
                    out.push(c);
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            b',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            b':' => out.extend_from_slice(b": "),
            _ => out.push(c),
        }
    }
    let cut = out.len() > limit || i < b.len();
    out.truncate(limit.min(out.len()));
    // Only ASCII was inserted into valid UTF-8, so a failure can only be a cut char.
    let text = match String::from_utf8(out) {
        Ok(s) => s,
        Err(e) => {
            let ok = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(ok);
            String::from_utf8(bytes).unwrap_or_default()
        }
    };
    (text, cut)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use sqler_driver_api::sqler_core::{Date, Decimal, Time, Timestamp, TimestampTz};

    use super::*;

    fn header(label: &'static str) -> ColumnHeader {
        ColumnHeader {
            name: "col".to_owned(),
            type_label: label,
        }
    }

    /// Summary, notes and lines of a detail, continued lines prefixed with `↪ `.
    fn render(d: &CellDetail) -> String {
        let mut out = format!("{} [{}]\n", d.summary(), d.type_label());
        for n in d.notes() {
            let _ = writeln!(out, "note: {n}");
        }
        for ix in 0..d.line_count() {
            let l = d.line(ix).unwrap();
            let _ = writeln!(out, "{}{}", if l.continued { "↪ " } else { "" }, l.text);
        }
        out
    }

    fn starts(text: &str) -> Vec<u32> {
        index_lines(text).0
    }

    fn of(label: &'static str, v: &Value) -> CellDetail {
        CellDetail::build_owned(&header(label), v, DetailCap::Bounded)
    }

    #[test]
    fn text_with_tabs_crlf_and_lone_cr() {
        let d = of(
            "text",
            &Value::Text("a\tb\r\n\tx\tyz\nlone\rcr\n\nlast".into()),
        );
        insta::assert_snapshot!(render(&d));
        assert_eq!(d.kind(), DetailKind::Text);
    }

    #[test]
    fn a_long_line_is_split_in_segments() {
        let line: String = ('a'..='z').cycle().take(2_500).collect();
        let d = of("text", &Value::Text(format!("{line}\nnext")));
        assert_eq!(d.line_count(), 4);
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn control_and_bidi_characters_are_made_visible() {
        let d = of(
            "text",
            &Value::Text(
                "nul\u{0}bel\u{7}esc\u{1b}del\u{7f}rlo\u{202e}evil\u{2066}x\u{200f}".into(),
            ),
        );
        insta::assert_snapshot!(render(&d));
        // The copy is the stored text.
        let v = Value::Text("a\u{202e}b\tc".into());
        assert_eq!(
            copy_text(CellRef::Value(&v)).as_deref(),
            Some("a\u{202e}b\tc")
        );
    }

    #[test]
    fn nested_json_is_re_indented() {
        let raw = r#" {"b" : [1, 2.50, 12345678901234567890123, {}, [], {"x":null}], "a":"q\"uo\\te\nüñ",
            "b":true, "e":{ }, "n":-0.0e+10 , "u":"é",
            "s":"a,{b}:[c]\u00e9\u0041 \"x{,}[:]\" \\", "w":"😀"} "#;
        let d = of("json", &Value::Json(raw.into()));
        assert_eq!(d.kind(), DetailKind::Json);
        insta::assert_snapshot!(render(&d));
    }

    #[test]
    fn invalid_or_deep_json_is_shown_raw_with_a_note() {
        let d = of("json", &Value::Json("{\"a\": [1, 2,}".into()));
        assert_eq!(d.kind(), DetailKind::RawJson);
        insta::assert_snapshot!(render(&d));
        let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
        let d = of("json", &Value::Json(deep.clone()));
        assert_eq!(d.kind(), DetailKind::RawJson);
        assert_eq!(d.line(0).unwrap().text, deep);
        let ok = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert_eq!(of("json", &Value::Json(ok)).kind(), DetailKind::Json);
        for (depth, kind) in [
            (127, DetailKind::Json),
            (128, DetailKind::RawJson),
            (100_000, DetailKind::RawJson),
        ] {
            let raw = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
            assert_eq!(of("json", &Value::Json(raw)).kind(), kind, "{depth}");
        }
    }

    #[test]
    fn binary_shows_partial_rows() {
        let bytes: Vec<u8> = (0..37u8)
            .map(|i| i.wrapping_mul(7).wrapping_add(30))
            .collect();
        let d = of("binary", &Value::Bytes(bytes));
        assert_eq!(d.line_count(), 3);
        insta::assert_snapshot!(render(&d));
        let e = of("binary", &Value::Bytes(Vec::new()));
        assert_eq!(e.line_count(), 0);
    }

    #[test]
    fn scalars_null_and_empty_text() {
        let date = Date::new(2024, 2, 29).unwrap();
        let time = Time::new(3, 4, 5, 500_000_000).unwrap();
        let ts = Timestamp::new(date, time);
        let dec: Decimal = "-12.50".parse().unwrap();
        let cases = [
            ("null", Value::Null),
            ("text", Value::Text(String::new())),
            ("date", Value::Date(date)),
            (
                "timestamptz",
                Value::TimestampTz(TimestampTz::new(ts, 3600).unwrap()),
            ),
            ("decimal", Value::Decimal(dec)),
            ("float", Value::Float(1.5)),
            ("boolean", Value::Bool(false)),
            ("integer", Value::Int(-42)),
        ];
        let mut all = String::new();
        for (label, v) in &cases {
            all.push_str(&render(&of(label, v)));
            all.push('\n');
        }
        insta::assert_snapshot!(all);
    }

    #[test]
    fn huge_text_is_capped_but_copied_whole() {
        let text = "é\n".repeat(5_000_000); // 15 MB, multi-byte
        let v = Value::Text(text.clone());
        let d = of("text", &v);
        assert!(d.shown_bytes() <= DETAIL_TEXT_CAP);
        assert!(d.is_truncated());
        assert_eq!(d.total_bytes(), text.len());
        assert!(d.notes()[0].starts_with("Showing the first 1.0 MiB of 14."));
        assert!(d.line(0).is_some());
        assert_eq!(
            copy_text(CellRef::Value(&v)).as_deref(),
            Some(text.as_str())
        );
        let full = CellDetail::build_owned(&header("text"), &v, DetailCap::Full);
        assert!(!full.is_truncated());
        assert_eq!(full.shown_bytes(), text.len());
        assert_eq!(full.line_count(), 5_000_000);
    }

    #[test]
    fn huge_binary_is_capped() {
        let v = Value::Bytes(vec![0xab; 10 * 1024 * 1024]);
        let d = of("binary", &v);
        assert_eq!(d.line_count(), DETAIL_BINARY_CAP / HEX_BYTES_PER_LINE);
        assert!(
            d.line(d.line_count() - 1)
                .unwrap()
                .text
                .starts_with("000ffff0")
        );
        assert!(d.is_truncated());
        assert_eq!(d.total_bytes(), 10 * 1024 * 1024);
        assert!(d.notes()[0].starts_with("Showing the first 1.0 MiB of 10.0 MiB"));
        let full = CellDetail::build_owned(&header("binary"), &v, DetailCap::Full);
        assert!(!full.is_truncated());
        assert_eq!(full.line_count(), 10 * 1024 * 1024 / HEX_BYTES_PER_LINE);
        let copied = copy_text(CellRef::Value(&v)).unwrap();
        assert_eq!(copied.len(), 2 + 2 * 10 * 1024 * 1024);
    }

    #[test]
    fn huge_json_is_raw_until_show_all() {
        let raw = format!("{{\"k\":\"{}\"}}", "x".repeat(5 * 1024 * 1024));
        let v = Value::Json(raw.clone());
        let d = of("json", &v);
        assert_eq!(d.kind(), DetailKind::RawJson);
        assert!(d.is_truncated());
        assert!(d.shown_bytes() <= DETAIL_TEXT_CAP);
        assert!(
            d.notes()
                .iter()
                .any(|n| n.starts_with("Too large to format here"))
        );
        let full = CellDetail::build_owned(&header("json"), &v, DetailCap::Full);
        assert_eq!(full.kind(), DetailKind::Json);
        assert!(!full.is_truncated());
        assert_eq!(
            full.line(1).unwrap().text,
            "  \"k\": \"".to_owned() + &"x".repeat(992)
        );
    }

    #[test]
    fn pretty_output_is_capped_too() {
        // Under the raw cap but over it once indented.
        let raw = format!("[{}]", "1,".repeat(450_000) + "1");
        assert!(raw.len() < JSON_PRETTY_CAP);
        let d = of("json", &Value::Json(raw));
        assert_eq!(d.kind(), DetailKind::Json);
        assert!(d.is_truncated());
        assert!(d.shown_bytes() <= DETAIL_TEXT_CAP);
        assert!(d.notes()[0].starts_with("Showing the first 1.0 MiB of the formatted value"));
    }

    #[test]
    fn build_owned_equals_build() {
        let date = Date::new(2024, 2, 29).unwrap();
        let dec: Decimal = "1.5".parse().unwrap();
        let values = [
            Value::Null,
            Value::Bool(true),
            Value::Int(7),
            Value::Float(2.5),
            Value::Text("a\tb\nc".into()),
            Value::Text(String::new()),
            Value::Bytes((0..40).collect()),
            Value::Json("{\"a\":[1,2]}".into()),
            Value::Json("{".into()),
            Value::Date(date),
            Value::Decimal(dec),
        ];
        for v in &values {
            let cell = match v {
                Value::Text(s) => CellRef::Text(s),
                Value::Json(s) => CellRef::Json(s),
                Value::Bytes(b) => CellRef::Bytes(b),
                Value::Null => CellRef::Null,
                Value::Bool(b) => CellRef::Bool(*b),
                Value::Int(i) => CellRef::Int(*i),
                Value::Float(x) => CellRef::Float(*x),
                other => CellRef::Value(other),
            };
            for cap in [DetailCap::Bounded, DetailCap::Full] {
                let a = CellDetail::build(&header("x"), cell, cap);
                let b = CellDetail::build_owned(&header("x"), v, cap);
                assert_eq!(render(&a), render(&b), "{v:?}");
                assert_eq!(a.kind(), b.kind());
                assert_eq!(a.total_bytes(), b.total_bytes());
                assert_eq!(a.is_truncated(), b.is_truncated());
            }
        }
    }

    #[test]
    fn cut_is_on_a_char_boundary() {
        let v = Value::Text("é".repeat(DETAIL_TEXT_CAP)); // 2 MiB, boundary falls between bytes
        let d = of("text", &v);
        assert!(d.shown_bytes() <= DETAIL_TEXT_CAP);
        assert_eq!(d.shown_bytes() % 2, 0);
    }

    #[test]
    fn copy_text_is_the_stored_value() {
        assert_eq!(copy_text(CellRef::Null), None);
        assert_eq!(copy_text(CellRef::Value(&Value::Null)), None);
        assert_eq!(copy_text(CellRef::Text("a\nb")).as_deref(), Some("a\nb"));
        assert_eq!(
            copy_text(CellRef::Json("{ \"a\":1 }")).as_deref(),
            Some("{ \"a\":1 }")
        );
        assert_eq!(
            copy_text(CellRef::Bytes(&[0xde, 0xad])).as_deref(),
            Some("0xdead")
        );
        assert_eq!(copy_text(CellRef::Int(-4)).as_deref(), Some("-4"));
        assert_eq!(copy_text(CellRef::Text("")).as_deref(), Some(""));
    }

    #[test]
    fn human_bytes_examples() {
        assert_eq!(human_bytes(0), "0 bytes");
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(4608), "4.5 KiB");
        assert_eq!(human_bytes(1 << 20), "1.0 MiB");
        assert_eq!(human_bytes(10 << 20), "10.0 MiB");
        assert_eq!(human_bytes(3 << 30), "3.0 GiB");
    }

    #[test]
    fn summary_groups_thousands() {
        let d = of("text", &Value::Text("x".repeat(12_345)));
        assert_eq!(d.summary(), "text · 12,345 bytes");
        assert_eq!(of("text", &Value::Null).summary(), "NULL");
    }

    #[test]
    fn debug_never_prints_the_value() {
        let secret = "hunter2-secret";
        let d = of("text", &Value::Text(secret.into()));
        assert!(!format!("{d:?}").contains(secret));
        let l = d.line(0).unwrap();
        assert!(!format!("{l:?}").contains(secret));
    }

    #[test]
    fn line_index_edge_cases() {
        assert_eq!(starts(""), Vec::<u32>::new());
        assert_eq!(starts("a\n"), vec![0]);
        assert_eq!(starts("a\n\n"), vec![0, 2]);
        assert_eq!(starts("\n"), vec![0]);
        let d = of("text", &Value::Text("\n".into()));
        assert_eq!(d.line(0).unwrap().text, "");
        assert!(d.line(1).is_none());
        // A line of exactly the segment length is not split.
        let exact = "y".repeat(DETAIL_LINE_CHARS);
        assert_eq!(starts(&exact), vec![0]);
        assert_eq!(starts(&format!("{exact}z")).len(), 2);
    }

    #[test]
    fn widest_line_is_found_for_the_horizontal_range() {
        let raw = format!("{{\"a\":1,\"long\":\"{}\",\"b\":2}}", "w".repeat(300));
        let d = of("json", &Value::Json(raw));
        assert_eq!(d.kind(), DetailKind::Json);
        assert_eq!(d.line(0).unwrap().text, "{");
        assert_eq!(d.widest_line(), Some(2));
        // First wins on a tie; a split segment counts by its own width.
        let t = of("text", &Value::Text("ab\ncd\n".into()));
        assert_eq!(t.widest_line(), Some(0));
        let long = of("text", &Value::Text(format!("x\n{}\ny", "z".repeat(1_500))));
        assert_eq!(long.widest_line(), Some(1));
        // Tabs count by the columns they take.
        let tabs = of("text", &Value::Text("abcdefghij\n\t\tx".into()));
        assert_eq!(tabs.widest_line(), Some(0));
        assert_eq!(of("text", &Value::Null).widest_line(), None);
        assert_eq!(of("text", &Value::Text(String::new())).widest_line(), None);
        assert_eq!(
            of("binary", &Value::Bytes(vec![1; 40])).widest_line(),
            Some(0)
        );
    }

    #[test]
    fn valid_json_with_out_of_range_numbers_is_formatted() {
        for raw in [
            "1e400",
            "{\"a\":1E+400}",
            "[1e999,-1e999,-0,-0.0,0e0]",
            "[123456789012345678901234567890,0.1000000000000000055511151231257827]",
        ] {
            let d = of("json", &Value::Json(raw.into()));
            assert_eq!(d.kind(), DetailKind::Json, "{raw}");
        }
        let d = of("json", &Value::Json("{\"a\":1E+400,\"b\":-0}".into()));
        let text: Vec<_> = (0..d.line_count())
            .map(|i| d.line(i).unwrap().text.into_owned())
            .collect();
        assert_eq!(text, ["{", "  \"a\": 1E+400,", "  \"b\": -0", "}"]);
        for raw in ["", "{", "[1,]", "1e", "{\"a\":1} x", "nul", "[01]", "\"a"] {
            assert_eq!(
                of("json", &Value::Json(raw.into())).kind(),
                DetailKind::RawJson,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn json_over_the_format_limit_is_never_formatted() {
        let raw = format!("[\"{}\"]", "x".repeat(JSON_FORMAT_MAX));
        let v = Value::Json(raw);
        for cap in [DetailCap::Bounded, DetailCap::Full] {
            let d = CellDetail::build_owned(&header("json"), &v, cap);
            assert_eq!(d.kind(), DetailKind::RawJson);
            assert!(
                d.notes()
                    .iter()
                    .any(|n| n == "Too large to format; shown as stored.")
            );
        }
        let full = CellDetail::build_owned(&header("json"), &v, DetailCap::Full);
        assert!(!full.is_truncated());
        assert_eq!(full.shown_bytes(), JSON_FORMAT_MAX + 4);
        // Still split by the 1,000 column rule.
        assert_eq!(full.line_count(), (JSON_FORMAT_MAX + 4).div_ceil(1_000));
    }

    #[test]
    fn no_line_past_the_end_of_a_binary() {
        for n in [0usize, 16, 32, 17] {
            let d = of("binary", &Value::Bytes(vec![7; n]));
            assert_eq!(d.line_count(), n.div_ceil(16));
            assert!(d.line(d.line_count()).is_none(), "{n} bytes");
            if n > 0 {
                assert!(d.line(d.line_count() - 1).is_some());
            }
        }
    }

    #[test]
    fn hex_copy_equals_display() {
        let bytes: Vec<u8> = (0..=255).collect();
        let want = CellRef::Bytes(&[]).to_string() + "";
        assert_eq!(want, "0x");
        for b in [&bytes[..], &[], &[0u8], &[0xde, 0xad]] {
            assert_eq!(
                copy_text(CellRef::Bytes(b)),
                Some(CellRef::Bytes(b).to_string())
            );
            let v = Value::Bytes(b.to_vec());
            assert_eq!(copy_text(CellRef::Value(&v)), Some(v.to_string()));
        }
    }

    #[test]
    fn tab_heavy_lines_are_capped_in_display_columns() {
        // 600 tabs are 2,400 columns: three segments of 250 tabs, 250 tabs, 100 tabs.
        let d = of("text", &Value::Text("\t".repeat(600)));
        assert_eq!(d.line_count(), 3);
        for ix in 0..d.line_count() {
            assert!(d.line(ix).unwrap().text.chars().count() <= DETAIL_LINE_CHARS);
        }
        assert_eq!(d.line(0).unwrap().text.chars().count(), 1_000);
        assert_eq!(d.widest_line(), Some(0));
        // A tab that does not fit starts the next segment.
        let t = of("text", &Value::Text(format!("{}\tz", "a".repeat(1_000))));
        assert_eq!(t.line_count(), 2);
        assert_eq!(t.line(1).unwrap().text, "    z");
    }

    #[test]
    fn c1_controls_are_substituted_in_the_display_only() {
        let s = "a\u{80}b\u{85}c\u{9f}d\u{a0}";
        let v = Value::Text(s.into());
        let d = of("text", &v);
        assert_eq!(
            d.line(0).unwrap().text,
            "a\u{fffd}b\u{fffd}c\u{fffd}d\u{a0}"
        );
        assert_eq!(copy_text(CellRef::Value(&v)).as_deref(), Some(s));
    }
}
