// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A dialect-aware SQL lexer that never fails.
//!
//! The tokens tile the input exactly: the first starts at 0, each starts where the previous one
//! ended, the last ends at `text.len()`, and every boundary is a char boundary. Unterminated
//! strings, identifiers and comments run to the end of the input and carry a flag.
//!
//! The lexer is a single forward pass with no recursion and no backtracking, so it is linear
//! in the input size.

// Short names (`s`, `b`, `d`, `c`) keep the byte-scanning code readable.
#![allow(clippy::many_single_char_names)]

use crate::Dialect;

/// The longest custom `MySQL` delimiter the lexer looks for, in bytes (the `mysql` client allows
/// 16 characters). It bounds the cost of the per-token delimiter check.
pub(crate) const MAX_DELIMITER_LEN: usize = 16;

/// What a token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// ASCII whitespace only: servers read non-ASCII whitespace as identifier characters.
    Whitespace,
    /// `-- ...` (and `# ...` on `MySQL`), up to before the line break.
    LineComment,
    /// `/* ... */`.
    BlockComment { terminated: bool },
    /// `MySQL` executable comment `/*! ... */`.
    ExecComment { terminated: bool },
    /// A string literal, with its prefix (`E'..'`, `N'..'`, `_utf8'..'`).
    Str { terminated: bool },
    /// A `PostgreSQL` dollar-quoted string.
    DollarStr { terminated: bool },
    /// A quoted identifier.
    QuotedIdent { terminated: bool },
    /// A keyword, identifier or `MySQL` variable.
    Word,
    /// A numeric literal.
    Number,
    /// A parameter placeholder: `$1`, `?`, `?1`, `:name`, `@name`, `$name`.
    Param,
    /// An operator.
    Op,
    /// `(`, `)`, `[`, `]`, `,`, `;` or `.`.
    Punct,
    /// The active custom delimiter (only when [`Lexer::set_delimiter`] was called).
    Delimiter,
    /// Any other single char (NUL, `\`, symbols).
    Other,
}

/// A token: a kind and a byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    /// The kind.
    pub kind: TokenKind,
    /// Start byte.
    pub start: usize,
    /// End byte (exclusive).
    pub end: usize,
}

impl Token {
    /// The token's text.
    pub(crate) fn text<'a>(&self, src: &'a str) -> &'a str {
        src.get(self.start..self.end).unwrap_or("")
    }

    /// Whitespace, a line comment or a (non-executable) block comment.
    pub(crate) fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment { .. }
        )
    }

    /// `false` for an unterminated string, identifier or comment.
    pub(crate) fn is_terminated(&self) -> bool {
        match self.kind {
            TokenKind::BlockComment { terminated }
            | TokenKind::ExecComment { terminated }
            | TokenKind::Str { terminated }
            | TokenKind::DollarStr { terminated }
            | TokenKind::QuotedIdent { terminated } => terminated,
            _ => true,
        }
    }

    /// Whether this is a [`TokenKind::Word`] equal to `upper` (ASCII case-insensitive).
    pub(crate) fn is_keyword(&self, src: &str, upper: &str) -> bool {
        self.kind == TokenKind::Word && self.text(src).eq_ignore_ascii_case(upper)
    }
}

/// Lexes the whole text. Never emits [`TokenKind::Delimiter`].
pub(crate) fn tokenize(dialect: Dialect, text: &str) -> Vec<Token> {
    Lexer::new(dialect, text).collect()
}

/// A pull lexer over one text.
pub(crate) struct Lexer<'a> {
    dialect: Dialect,
    src: &'a str,
    pos: usize,
    delimiter: Option<&'a str>,
}

impl<'a> Lexer<'a> {
    /// A lexer at the start of `src`.
    pub(crate) fn new(dialect: Dialect, src: &'a str) -> Self {
        Self {
            dialect,
            src,
            pos: 0,
            delimiter: None,
        }
    }

    /// Sets (or clears) the custom delimiter, checked at the start of every token.
    /// Delimiters longer than [`MAX_DELIMITER_LEN`] bytes and empty ones are ignored.
    pub(crate) fn set_delimiter(&mut self, delimiter: Option<&'a str>) {
        self.delimiter = delimiter.filter(|d| !d.is_empty() && d.len() <= MAX_DELIMITER_LEN);
    }

    /// The byte offset of the next token.
    #[cfg(test)]
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Moves to `pos`, clamped to the text and rounded up to a char boundary.
    pub(crate) fn seek(&mut self, pos: usize) {
        let mut pos = pos.min(self.src.len());
        while !self.src.is_char_boundary(pos) {
            pos += 1;
        }
        self.pos = pos;
    }

    fn lex_at(&self, start: usize, c: char) -> (TokenKind, usize) {
        let s = self.src;
        let b = s.as_bytes();
        let d = self.dialect;
        let next = b.get(start + 1).copied();
        if let Some(delim) = self.delimiter
            && s[start..].starts_with(delim)
        {
            return (TokenKind::Delimiter, start + delim.len());
        }
        if is_ws(c) {
            let end = s[start..]
                .char_indices()
                .find(|&(_, ch)| !is_ws(ch))
                .map_or(s.len(), |(i, _)| start + i);
            return (TokenKind::Whitespace, end);
        }
        if c == '[' && d == Dialect::Sqlite {
            // `SQLite` bracket identifiers end at the first `]`; there is no doubling.
            let end = s[start + 1..].find(']');
            let kind = TokenKind::QuotedIdent {
                terminated: end.is_some(),
            };
            return (kind, end.map_or(s.len(), |i| start + 1 + i + 1));
        }
        match c {
            '-' if next == Some(b'-')
                && (d != Dialect::MySql || self.mysql_dash_comment(start)) =>
            {
                (TokenKind::LineComment, self.line_end(start))
            }
            '#' if d == Dialect::MySql => (TokenKind::LineComment, self.line_end(start)),
            '/' if next == Some(b'*') => {
                if d == Dialect::MySql && b.get(start + 2) == Some(&b'!') {
                    let (end, terminated) = self.block_comment(start, false);
                    (TokenKind::ExecComment { terminated }, end)
                } else {
                    let (end, terminated) = self.block_comment(start, d == Dialect::Postgres);
                    (TokenKind::BlockComment { terminated }, end)
                }
            }
            '\'' => {
                let (end, terminated) = self.quoted(start + 1, b'\'', d == Dialect::MySql);
                (TokenKind::Str { terminated }, end)
            }
            '"' => {
                if d == Dialect::MySql {
                    let (end, terminated) = self.quoted(start + 1, b'"', true);
                    (TokenKind::Str { terminated }, end)
                } else {
                    let (end, terminated) = self.quoted(start + 1, b'"', false);
                    (TokenKind::QuotedIdent { terminated }, end)
                }
            }
            '`' if d != Dialect::Postgres => {
                let (end, terminated) = self.quoted(start + 1, b'`', false);
                (TokenKind::QuotedIdent { terminated }, end)
            }
            '0'..='9' => (TokenKind::Number, self.number(start)),
            '.' if next.is_some_and(|n| n.is_ascii_digit()) => {
                (TokenKind::Number, self.number(start))
            }
            ';' | '(' | ')' | '[' | ']' | ',' | '.' => (TokenKind::Punct, start + 1),
            '$' => self.dollar(start),
            '?' => self.question(start),
            ':' => self.colon(start),
            '@' => self.at(start),
            c if c == '_' || c.is_alphabetic() => self.word(start),
            _ => self.operator(start, c),
        }
    }

    /// `--` on `MySQL` is a comment only when followed by an ASCII whitespace or control byte, or
    /// EOF. The server tests the next byte with ASCII ctype, so non-ASCII chars do not count.
    fn mysql_dash_comment(&self, start: usize) -> bool {
        self.src
            .as_bytes()
            .get(start + 2)
            .is_none_or(|&b| b <= 0x20 || b == 0x7f)
    }

    /// The end of the line that contains `from`: before `\r` or `\n`, or the end of the text.
    fn line_end(&self, from: usize) -> usize {
        self.src[from..]
            .find(['\r', '\n'])
            .map_or(self.src.len(), |i| from + i)
    }

    /// Scans a block comment that starts at `start` with `/*`.
    fn block_comment(&self, start: usize, nested: bool) -> (usize, bool) {
        let b = self.src.as_bytes();
        let mut i = start + 2;
        let mut depth = 1usize;
        while i < b.len() {
            if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return (i, true);
                }
            } else if nested && b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                depth += 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        (b.len(), false)
    }

    /// Scans the rest of a quoted token; `from` is just after the opening quote.
    /// The quote doubles itself; with `backslash` a backslash escapes the next char.
    /// Returns the end and whether the closing quote was found.
    fn quoted(&self, from: usize, quote: u8, backslash: bool) -> (usize, bool) {
        let b = self.src.as_bytes();
        let mut i = from;
        while i < b.len() {
            let byte = b[i];
            if backslash && byte == b'\\' {
                i += 2;
            } else if byte == quote {
                if b.get(i + 1) == Some(&quote) {
                    i += 2;
                } else {
                    return (i + 1, true);
                }
            } else {
                i += 1;
            }
        }
        (b.len(), false)
    }

    fn number(&self, start: usize) -> usize {
        let b = self.src.as_bytes();
        let mut i = start;
        let mut after_e = false;
        while i < b.len() {
            let byte = b[i];
            if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' {
                after_e = byte == b'e' || byte == b'E';
                i += 1;
            } else if (byte == b'+' || byte == b'-') && after_e {
                after_e = false;
                i += 1;
            } else {
                break;
            }
        }
        i
    }

    /// Consumes `[alphanumeric _]` chars from `from`; returns the end.
    fn name_end(&self, from: usize) -> usize {
        self.src[from..]
            .char_indices()
            .find(|&(_, ch)| !(ch == '_' || ch.is_alphanumeric()))
            .map_or(self.src.len(), |(i, _)| from + i)
    }

    fn digits_end(&self, from: usize) -> usize {
        let b = self.src.as_bytes();
        let mut i = from;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        i
    }

    fn dollar(&self, start: usize) -> (TokenKind, usize) {
        let s = self.src;
        let b = s.as_bytes();
        match self.dialect {
            Dialect::Postgres => {
                if b.get(start + 1).is_some_and(u8::is_ascii_digit) {
                    return (TokenKind::Param, self.digits_end(start + 1));
                }
                // `$tag$` or `$$`: the tag is an identifier that does not start with a digit.
                let tag_start = start + 1;
                let mut tag_end = tag_start;
                for (i, ch) in s[tag_start..].char_indices() {
                    let ok = if i == 0 {
                        ch == '_' || ch.is_alphabetic()
                    } else {
                        ch == '_' || ch.is_alphanumeric()
                    };
                    if !ok {
                        break;
                    }
                    tag_end = tag_start + i + ch.len_utf8();
                }
                if b.get(tag_end) == Some(&b'$') {
                    let open_end = tag_end + 1;
                    let opener = &s[start..open_end];
                    return match s[open_end..].find(opener) {
                        Some(p) => (
                            TokenKind::DollarStr { terminated: true },
                            open_end + p + opener.len(),
                        ),
                        None => (TokenKind::DollarStr { terminated: false }, s.len()),
                    };
                }
                (TokenKind::Other, start + 1)
            }
            Dialect::Sqlite => {
                let end = self.name_end(start + 1);
                if end > start + 1 {
                    (TokenKind::Param, end)
                } else {
                    (TokenKind::Other, start + 1)
                }
            }
            Dialect::MySql => (TokenKind::Other, start + 1),
        }
    }

    fn question(&self, start: usize) -> (TokenKind, usize) {
        match self.dialect {
            Dialect::Postgres => (TokenKind::Op, self.pg_op_end(start)),
            Dialect::MySql => (TokenKind::Param, start + 1),
            Dialect::Sqlite => (TokenKind::Param, self.digits_end(start + 1)),
        }
    }

    fn colon(&self, start: usize) -> (TokenKind, usize) {
        let b = self.src.as_bytes();
        let next = b.get(start + 1).copied();
        if next == Some(b'=') || (self.dialect == Dialect::Postgres && next == Some(b':')) {
            return (TokenKind::Op, start + 2);
        }
        let after_colon = start > 0 && b[start - 1] == b':';
        if !after_colon && next.is_some_and(|n| n.is_ascii_alphabetic() || n == b'_') {
            return (TokenKind::Param, self.name_end(start + 1));
        }
        (TokenKind::Other, start + 1)
    }

    fn at(&self, start: usize) -> (TokenKind, usize) {
        let s = self.src;
        match self.dialect {
            Dialect::Postgres => (TokenKind::Op, self.pg_op_end(start)),
            Dialect::Sqlite => {
                let end = self.name_end(start + 1);
                if end > start + 1 {
                    (TokenKind::Param, end)
                } else {
                    (TokenKind::Other, start + 1)
                }
            }
            Dialect::MySql => {
                let b = s.as_bytes();
                let double = b.get(start + 1) == Some(&b'@');
                let name_start = start + if double { 2 } else { 1 };
                if !double
                    && let Some(&q) = b.get(name_start)
                    && matches!(q, b'\'' | b'"' | b'`')
                {
                    let (end, terminated) = self.quoted(name_start + 1, q, q != b'`');
                    let kind = if q == b'`' {
                        TokenKind::QuotedIdent { terminated }
                    } else {
                        TokenKind::Str { terminated }
                    };
                    return (kind, end);
                }
                let end = s[name_start..]
                    .char_indices()
                    .find(|&(_, ch)| !(ch == '_' || ch == '$' || ch == '.' || ch.is_alphanumeric()))
                    .map_or(s.len(), |(i, _)| name_start + i);
                if end > name_start {
                    (TokenKind::Word, end)
                } else {
                    (TokenKind::Other, start + 1)
                }
            }
        }
    }

    fn word(&self, start: usize) -> (TokenKind, usize) {
        let s = self.src;
        let b = s.as_bytes();
        let d = self.dialect;
        let dollar_ok = d != Dialect::Sqlite;
        let mut end = s.len();
        for (i, ch) in s[start..].char_indices() {
            let cont = ch == '_' || ch.is_alphanumeric() || (ch == '$' && dollar_ok);
            if !cont {
                end = start + i;
                break;
            }
            if i > 0
                && let Some(delim) = self.delimiter
                && s[start + i..].starts_with(delim)
            {
                end = start + i;
                break;
            }
        }
        let w = &s[start..end];
        let next = b.get(end).copied();
        let ci = |x: &str| w.eq_ignore_ascii_case(x);
        if next == Some(b'\'') {
            let backslash = match d {
                Dialect::Postgres if ci("E") => Some(true),
                Dialect::Postgres if ci("B") || ci("X") || ci("N") => Some(false),
                Dialect::MySql if ci("X") || ci("B") => Some(false),
                Dialect::MySql if ci("N") => Some(true),
                Dialect::MySql if w.len() > 1 && w.starts_with('_') => Some(true),
                Dialect::Sqlite if ci("X") => Some(false),
                _ => None,
            };
            if let Some(backslash) = backslash {
                let (e, terminated) = self.quoted(end + 1, b'\'', backslash);
                return (TokenKind::Str { terminated }, e);
            }
        }
        if d == Dialect::Postgres && ci("U") && b.get(end) == Some(&b'&') {
            match b.get(end + 1) {
                Some(&b'\'') => {
                    let (e, terminated) = self.quoted(end + 2, b'\'', false);
                    return (TokenKind::Str { terminated }, e);
                }
                Some(&b'"') => {
                    let (e, terminated) = self.quoted(end + 2, b'"', false);
                    return (TokenKind::QuotedIdent { terminated }, e);
                }
                _ => {}
            }
        }
        (TokenKind::Word, end)
    }

    fn pg_op_end(&self, start: usize) -> usize {
        let b = self.src.as_bytes();
        let mut i = start;
        while i < b.len() {
            let byte = b[i];
            if !is_pg_op_byte(byte) {
                break;
            }
            if i > start
                && ((byte == b'-' && b.get(i + 1) == Some(&b'-'))
                    || (byte == b'/' && b.get(i + 1) == Some(&b'*')))
            {
                break;
            }
            i += 1;
        }
        i.max(start + 1)
    }

    fn operator(&self, start: usize, c: char) -> (TokenKind, usize) {
        if self.dialect == Dialect::Postgres {
            return if is_pg_op_byte(u8::try_from(c).unwrap_or(0)) {
                (TokenKind::Op, self.pg_op_end(start))
            } else {
                (TokenKind::Other, start + c.len_utf8())
            };
        }
        let rest = &self.src[start..];
        if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
            return (TokenKind::Op, start + op.len());
        }
        if matches!(
            c,
            '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' | '~' | '^' | '&' | '|'
        ) {
            (TokenKind::Op, start + 1)
        } else {
            (TokenKind::Other, start + c.len_utf8())
        }
    }
}

/// The `MySQL`/`SQLite` multi-char operators, longest first.
const OPS: [&str; 12] = [
    "<=>", "->>", "->", "<=", ">=", "<>", "!=", "||", "&&", "<<", ">>", "==",
];

/// ASCII whitespace: space, tab, LF, VT, FF, CR.
pub(crate) fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

fn is_pg_op_byte(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'<'
            | b'>'
            | b'='
            | b'~'
            | b'!'
            | b'@'
            | b'#'
            | b'%'
            | b'^'
            | b'&'
            | b'|'
            | b'`'
            | b'?'
    )
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let start = self.pos;
        let c = self.src.get(start..)?.chars().next()?;
        let (kind, end) = self.lex_at(start, c);
        // Defence: always make progress and stay on a char boundary.
        let min_end = start + c.len_utf8();
        let end = if end < min_end || end > self.src.len() || !self.src.is_char_boundary(end) {
            min_end
        } else {
            end
        };
        self.pos = end;
        Some(Token { kind, start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: Dialect, s: &str) -> Vec<(TokenKind, &str)> {
        tokenize(d, s)
            .into_iter()
            .map(|t| (t.kind, &s[t.start..t.end]))
            .collect()
    }

    fn sig(d: Dialect, s: &str) -> Vec<(TokenKind, &str)> {
        kinds(d, s)
            .into_iter()
            .filter(|(k, _)| *k != TokenKind::Whitespace)
            .collect()
    }

    fn one(d: Dialect, s: &str) -> TokenKind {
        let t = tokenize(d, s);
        assert_eq!(t.len(), 1, "{s:?} -> {t:?}");
        t[0].kind
    }

    use crate::Dialect::{MySql as My, Postgres as Pg, Sqlite as Lite};
    use TokenKind::{
        BlockComment, DollarStr, ExecComment, LineComment, Number, Op, Other, Param, Punct,
        QuotedIdent, Str, Whitespace, Word,
    };

    #[test]
    fn tiles_and_whitespace() {
        for d in Dialect::ALL {
            let s = "\u{FEFF} a\u{a0}\tb\u{3000}";
            let t = tokenize(d, s);
            assert_eq!(t.first().map(|t| t.start), Some(0));
            assert_eq!(t.last().map(|t| t.end), Some(s.len()));
            assert!(t.windows(2).all(|w| w[0].end == w[1].start));
            assert_eq!(one(d, " \t\n\r\x0B\x0C"), Whitespace);
            // Non-ASCII whitespace is not a separator: servers read it as identifier chars.
            for c in ['\u{FEFF}', '\u{a0}', '\u{85}', '\u{3000}'] {
                assert_eq!(one(d, &c.to_string()), Other, "{c:?}");
            }
        }
        assert_eq!(tokenize(Pg, "").len(), 0);
    }

    #[test]
    fn line_comments() {
        assert_eq!(sig(Pg, "a -- x\nb")[1], (LineComment, "-- x"));
        assert_eq!(sig(Lite, "--x\r\nb")[0], (LineComment, "--x"));
        // MySQL: `--` needs whitespace, a control char or EOF after it.
        assert_eq!(one(My, "-- x"), LineComment);
        assert_eq!(one(My, "--"), LineComment);
        assert_eq!(sig(My, "--x")[0], (Op, "-"));
        assert_eq!(one(My, "--\u{1}"), LineComment);
        assert_eq!(one(My, "# x"), LineComment);
        // Non-ASCII whitespace or control chars after `--` do not start a comment on MySQL.
        for c in ['\u{a0}', '\u{85}', '\u{3000}', '\u{FEFF}'] {
            let text = format!("--{c}x");
            assert_eq!(sig(My, &text)[0], (Op, "-"), "{c:?}");
        }
        assert_ne!(one(Pg, "#"), LineComment);
        assert_ne!(one(Lite, "#"), LineComment);
    }

    #[test]
    fn sqlite_brackets_do_not_double() {
        assert_eq!(
            sig(Lite, "[a]] b"),
            vec![
                (QuotedIdent { terminated: true }, "[a]"),
                (Punct, "]"),
                (Word, "b")
            ]
        );
        assert_eq!(one(Lite, "[a b"), QuotedIdent { terminated: false });
    }

    #[test]
    fn mysql_hex_and_bit_strings_have_no_backslash_escape() {
        assert_eq!(sig(My, r"X'\' a").len(), 2);
        assert_eq!(sig(My, r"B'\' a").len(), 2);
        assert_eq!(sig(My, r"N'\'' x' a").len(), 2);
    }

    #[test]
    fn block_comments() {
        assert_eq!(one(Lite, "/* ; */"), BlockComment { terminated: true });
        assert_eq!(one(Lite, "/* /* x */"), BlockComment { terminated: true });
        assert_eq!(
            sig(Lite, "/* /* x */ */")[0],
            (BlockComment { terminated: true }, "/* /* x */")
        );
        assert_eq!(one(Pg, "/* /* x */ */"), BlockComment { terminated: true });
        assert_eq!(one(Pg, "/* /* x */"), BlockComment { terminated: false });
        assert_eq!(one(Pg, "/*/"), BlockComment { terminated: false });
        assert_eq!(one(My, "/*! x */"), ExecComment { terminated: true });
        assert_eq!(one(My, "/*!50000 x"), ExecComment { terminated: false });
        assert_eq!(one(Pg, "/*! x */"), BlockComment { terminated: true });
    }

    #[test]
    fn strings() {
        for d in Dialect::ALL {
            assert_eq!(one(d, "'a;b'"), Str { terminated: true });
            assert_eq!(one(d, "'it''s'"), Str { terminated: true });
            assert_eq!(one(d, "'abc"), Str { terminated: false });
        }
        // Backslash escapes: MySQL always, PG only with E, SQLite never.
        assert_eq!(one(My, r"'a\'b'"), Str { terminated: true });
        assert_eq!(sig(Pg, r"'a\'b'").len(), 2);
        assert_eq!(sig(Lite, r"'a\'b'")[0].1, r"'a\'");
        assert_eq!(one(Pg, r"E'a\'b'"), Str { terminated: true });
        assert_eq!(one(Pg, r"e'\''"), Str { terminated: true });
        assert_eq!(one(Pg, r"E'a\'"), Str { terminated: false });
        assert_eq!(one(Pg, r"'\'"), Str { terminated: true });
        // MySQL double quote is a string.
        assert_eq!(one(My, r#""a;b""#), Str { terminated: true });
        assert_eq!(one(My, r#""a\"b""#), Str { terminated: true });
    }

    #[test]
    fn string_prefixes() {
        for p in ["E", "B", "X", "N", "U&", "e", "x"] {
            assert_eq!(one(Pg, &format!("{p}'x'")), Str { terminated: true }, "{p}");
        }
        assert_eq!(one(Pg, "U&\"x\""), QuotedIdent { terminated: true });
        assert_eq!(one(Pg, "U&'x'"), Str { terminated: true });
        assert_eq!(sig(Pg, "Z'x'").len(), 2);
        for p in ["N", "X", "B", "n", "_utf8mb4", "_latin1"] {
            assert_eq!(one(My, &format!("{p}'x'")), Str { terminated: true }, "{p}");
        }
        assert_eq!(sig(My, "E'x'").len(), 2);
        assert_eq!(sig(My, "_'x'").len(), 2);
        assert_eq!(one(Lite, "X'ff'"), Str { terminated: true });
        assert_eq!(one(Lite, "x'ff'"), Str { terminated: true });
        assert_eq!(sig(Lite, "N'x'").len(), 2);
    }

    #[test]
    fn quoted_identifiers() {
        assert_eq!(one(Pg, "\"a;\"\"b\""), QuotedIdent { terminated: true });
        assert_eq!(one(Lite, "\"a\""), QuotedIdent { terminated: true });
        assert_eq!(one(My, "`a``b`"), QuotedIdent { terminated: true });
        assert_eq!(one(Lite, "`a`"), QuotedIdent { terminated: true });
        assert_eq!(one(Lite, "[a;b]"), QuotedIdent { terminated: true });
        assert_eq!(one(Lite, "[a;b"), QuotedIdent { terminated: false });
        assert_eq!(one(Pg, "\"abc"), QuotedIdent { terminated: false });
        // Brackets are punctuation elsewhere; the backtick is a PG operator char.
        assert_eq!(sig(Pg, "[a]")[0], (Punct, "["));
        assert_eq!(sig(My, "[a]")[0], (Punct, "["));
        assert_eq!(one(Pg, "`"), Op);
    }

    #[test]
    fn dollar_quotes() {
        assert_eq!(one(Pg, "$$;$$"), DollarStr { terminated: true });
        assert_eq!(one(Pg, "$f$ $$ ; $f$"), DollarStr { terminated: true });
        assert_eq!(one(Pg, "$f$ $g$ x"), DollarStr { terminated: false });
        assert_eq!(one(Pg, "$é$x$é$"), DollarStr { terminated: true });
        assert_eq!(one(Pg, "$1"), Param);
        assert_eq!(sig(Pg, "$1$a$")[0], (Param, "$1"));
        // A `$` inside a word does not open a quote.
        assert_eq!(sig(Pg, "a$$b$$"), vec![(Word, "a$$b$$")]);
        assert_eq!(sig(Pg, "$ 1")[0], (Other, "$"));
        assert_eq!(sig(My, "$$").len(), 2);
        assert_eq!(sig(Lite, "$$").len(), 2);
    }

    #[test]
    fn words() {
        assert_eq!(sig(Pg, "foo_1 é名x"), vec![(Word, "foo_1"), (Word, "é名x")]);
        assert_eq!(one(Pg, "_a"), Word);
        assert_eq!(one(Pg, "a$b"), Word);
        assert_eq!(one(My, "a$b"), Word);
        assert_eq!(sig(Lite, "a$b").len(), 2);
        assert_eq!(one(My, "@x"), Word);
        assert_eq!(one(My, "@@global.x"), Word);
        assert_eq!(one(My, "@'q'"), Str { terminated: true });
        assert_eq!(one(My, "@`q`"), QuotedIdent { terminated: true });
        assert_eq!(sig(My, "'u'@'h'")[1], (Str { terminated: true }, "@'h'"));
        assert_eq!(one(My, "@'q"), Str { terminated: false });
        assert_eq!(one(My, "@\"q\\\""), Str { terminated: false });
        assert_eq!(one(My, "@`q``"), QuotedIdent { terminated: false });
        assert_eq!(one(My, "@"), Other);
    }

    #[test]
    fn numbers() {
        for s in [
            "1", "1.5", ".5", "1e10", "1e+10", "1E-5", "0xFF", "1_000", "1.5e-3",
        ] {
            assert_eq!(one(Pg, s), Number, "{s}");
        }
        assert_eq!(sig(Pg, "1+2").len(), 3);
        assert_eq!(sig(Pg, "t.a")[1], (Punct, "."));
    }

    #[test]
    fn params() {
        assert_eq!(one(Pg, "$12"), Param);
        assert_eq!(one(My, "?"), Param);
        assert_eq!(one(Lite, "?"), Param);
        assert_eq!(sig(Lite, "?12")[0], (Param, "?12"));
        assert_eq!(sig(My, "?1").len(), 2);
        assert_eq!(one(Lite, "@name"), Param);
        assert_eq!(one(Lite, "$name"), Param);
        for d in Dialect::ALL {
            assert_eq!(sig(d, ":name_1")[0], (Param, ":name_1"));
            assert_eq!(sig(d, ":_x")[0], (Param, ":_x"));
            assert_ne!(sig(d, ":1")[0].0, Param);
        }
        // Not a param after a colon; PG lexes `::` first.
        assert_eq!(
            sig(Pg, "x::int"),
            vec![(Word, "x"), (Op, "::"), (Word, "int")]
        );
        assert!(sig(Lite, "x::int").iter().all(|(k, _)| *k != Param));
        assert!(sig(My, "x::int").iter().all(|(k, _)| *k != Param));
        assert_eq!(sig(Pg, "a:=b")[1], (Op, ":="));
        assert_eq!(sig(My, "a:=b")[1], (Op, ":="));
        assert_eq!(sig(Pg, "a[1:n]")[3], (Param, ":n"));
        // PG: `?` and `@` are operators.
        assert_eq!(sig(Pg, "a ?| b")[1], (Op, "?|"));
        assert_eq!(sig(Pg, "a @> b")[1], (Op, "@>"));
    }

    #[test]
    fn operators() {
        assert_eq!(sig(Pg, "a<=b")[1], (Op, "<="));
        assert_eq!(sig(Pg, "a->>b")[1], (Op, "->>"));
        assert_eq!(sig(Pg, "a #>> b")[1], (Op, "#>>"));
        // The run stops before a comment start.
        assert_eq!(sig(Pg, "a+--c")[1], (Op, "+"));
        assert_eq!(sig(Pg, "a+/*c*/")[1], (Op, "+"));
        for (s, op) in [
            ("<=>", "<=>"),
            ("->>", "->>"),
            ("->", "->"),
            ("<=", "<="),
            (">=", ">="),
            ("<>", "<>"),
            ("!=", "!="),
            ("||", "||"),
            ("&&", "&&"),
            ("<<", "<<"),
            (">>", ">>"),
            (":=", ":="),
            ("==", "=="),
        ] {
            assert_eq!(sig(My, s), vec![(Op, op)], "{s}");
            assert_eq!(sig(Lite, s), vec![(Op, op)], "{s}");
        }
        assert_eq!(sig(My, "+-").len(), 2);
        assert_eq!(sig(Lite, "***").len(), 3);
        assert_eq!(sig(Lite, "~")[0].0, Op);
    }

    #[test]
    fn punct_and_other() {
        assert_eq!(
            sig(Pg, "(),;[]."),
            (0..7)
                .map(|i| (Punct, &"(),;[]."[i..=i]))
                .collect::<Vec<_>>()
        );
        for s in ["\0", "\\", "é", "名", "\u{1}"] {
            assert_eq!(
                one(Pg, s),
                if s == "é" || s == "名" { Word } else { Other },
                "{s:?}"
            );
        }
        assert_eq!(one(My, "\\"), Other);
        assert_eq!(one(Lite, "\0"), Other);
        assert_eq!(one(Pg, "§"), Other);
    }

    #[test]
    fn delimiter_tokens() {
        let src = "a//b //";
        let mut lx = Lexer::new(My, src);
        lx.set_delimiter(Some("//"));
        let t: Vec<_> = lx.by_ref().collect();
        assert_eq!(
            t.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![
                Word,
                TokenKind::Delimiter,
                Word,
                Whitespace,
                TokenKind::Delimiter
            ]
        );
        // `END$$` splits the word at the delimiter.
        let mut lx = Lexer::new(My, "END$$");
        lx.set_delimiter(Some("$$"));
        assert_eq!(lx.next().map(|t| t.end), Some(3));
        // Over-long delimiters are ignored.
        let long = "x".repeat(MAX_DELIMITER_LEN + 1);
        let mut lx = Lexer::new(My, &long);
        lx.set_delimiter(Some(&long));
        assert_eq!(lx.next().map(|t| t.kind), Some(Word));
    }

    #[test]
    fn seek_and_position() {
        let mut lx = Lexer::new(Pg, "ab é");
        lx.seek(3);
        assert_eq!(lx.position(), 3);
        lx.seek(4); // inside `é`
        assert_eq!(lx.position(), 5);
        lx.seek(99);
        assert_eq!(lx.position(), 5);
        assert!(lx.next().is_none());
    }

    #[test]
    fn token_helpers() {
        let s = "Select 'x";
        let t = tokenize(Pg, s);
        assert!(t[0].is_keyword(s, "SELECT"));
        assert!(!t[2].is_keyword(s, "X"));
        assert!(t[1].is_trivia() && !t[0].is_trivia());
        assert!(t[0].is_terminated() && !t[2].is_terminated());
        assert_eq!(t[0].text(s), "Select");
    }
}
