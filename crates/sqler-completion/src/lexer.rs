// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A hand-written, total SQL lexer.
//!
//! It never fails: unterminated strings, quoted identifiers and comments run to the end of the
//! text and are flagged. Spans are byte offsets that always sit on char boundaries. Whitespace
//! produces no token.

use sqler_sql::Dialect;

use crate::dialect_data::{Flavor, flavor};

/// The kind of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokKind {
    /// A bare word: keyword or unquoted identifier.
    Word,
    /// A quoted identifier (`"x"`, `` `x` ``, `[x]`).
    Quoted,
    /// A string literal (including dollar-quoted strings).
    Str,
    /// A number or a positional parameter such as `$1`.
    Number,
    /// A line or block comment.
    Comment,
    /// One punctuation or operator character.
    Punct,
}

/// A token with its byte span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
    /// The token has no closing delimiter, so text typed at its end extends it. Line comments
    /// are always open-ended.
    pub unterminated: bool,
}

impl Token {
    /// The token text; empty if the span is somehow not on char boundaries.
    pub(crate) fn text<'a>(&self, s: &'a str) -> &'a str {
        s.get(self.start..self.end).unwrap_or("")
    }

    /// Whether this is the single punctuation character `c`.
    pub(crate) fn is_punct(&self, s: &str, c: char) -> bool {
        self.kind == TokKind::Punct && self.text(s).starts_with(c)
    }
}

/// Splits `text` into tokens.
// Byte-level scanning reads best with the usual short names (`b`, `n`, `i`, `c`).
#[allow(clippy::many_single_char_names)]
pub(crate) fn lex(text: &str, dialect: Dialect) -> Vec<Token> {
    let flavor = flavor(dialect);
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n / 4 + 4);
    let mut i = 0;
    while i < n {
        let c = b[i];
        let (kind, end, unterminated) = match c {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c => {
                i += 1;
                continue;
            }
            b'-' if b.get(i + 1) == Some(&b'-')
                && (flavor != Flavor::MySql
                    || matches!(b.get(i + 2), None | Some(b' ' | b'\t' | b'\n' | b'\r'))) =>
            {
                (TokKind::Comment, line_end(b, i), true)
            }
            b'#' if flavor == Flavor::MySql => (TokKind::Comment, line_end(b, i), true),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let (end, open) =
                    block_comment(b, i, matches!(flavor, Flavor::Postgres | Flavor::TSql));
                (TokKind::Comment, end, open)
            }
            b'\'' => {
                let (end, open) = scan_quoted(b, i, b'\'', flavor == Flavor::MySql);
                (TokKind::Str, end, open)
            }
            b'"' if flavor == Flavor::MySql => {
                let (end, open) = scan_quoted(b, i, b'"', true);
                (TokKind::Str, end, open)
            }
            b'"' => {
                let (end, open) = scan_quoted(b, i, b'"', false);
                (TokKind::Quoted, end, open)
            }
            b'`' if matches!(flavor, Flavor::MySql | Flavor::Sqlite) => {
                let (end, open) = scan_quoted(b, i, b'`', false);
                (TokKind::Quoted, end, open)
            }
            b'[' if flavor == Flavor::TSql => bracket_ident(b, i),
            b'[' if flavor == Flavor::Sqlite => match b[i + 1..].iter().position(|&x| x == b']') {
                Some(p) => (TokKind::Quoted, i + 1 + p + 1, false),
                None => (TokKind::Quoted, n, true),
            },
            b'$' if flavor == Flavor::Postgres => dollar(text, i),
            b'$' | b'@' | b'#' if flavor == Flavor::TSql => tsql_sigil(text, i),
            b'0'..=b'9' => (TokKind::Number, number_end(b, i), false),
            _ => {
                let Some(ch) = text.get(i..).and_then(|s| s.chars().next()) else {
                    i += 1;
                    continue;
                };
                if ch.is_whitespace() {
                    i += ch.len_utf8();
                    continue;
                }
                if ch.is_alphabetic() || ch == '_' {
                    let end = if flavor == Flavor::TSql {
                        tsql_word_end(text, i)
                    } else {
                        word_end(text, i)
                    };
                    let is_e = flavor == Flavor::Postgres
                        && end == i + 1
                        && (c == b'E' || c == b'e')
                        && b.get(end) == Some(&b'\'');
                    let is_n = flavor == Flavor::TSql
                        && end == i + 1
                        && (c == b'N' || c == b'n')
                        && b.get(end) == Some(&b'\'');
                    if is_e || is_n {
                        let (end, open) = scan_quoted(b, end, b'\'', is_e);
                        (TokKind::Str, end, open)
                    } else {
                        (TokKind::Word, end, false)
                    }
                } else {
                    (TokKind::Punct, i + ch.len_utf8(), false)
                }
            }
        };
        out.push(Token {
            kind,
            start: i,
            end,
            unterminated,
        });
        i = end;
    }
    out
}

fn line_end(b: &[u8], from: usize) -> usize {
    b[from..]
        .iter()
        .position(|&x| x == b'\n' || x == b'\r')
        .map_or(b.len(), |p| from + p)
}

/// Returns the end of a block comment starting at `from` and whether it is unterminated.
fn block_comment(b: &[u8], from: usize, nested: bool) -> (usize, bool) {
    let n = b.len();
    let mut depth = 1usize;
    let mut j = from + 2;
    while j < n {
        if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return (j, false);
            }
        } else if nested && b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
            depth += 1;
            j += 2;
        } else {
            j += 1;
        }
    }
    (n, true)
}

/// Returns the end of a quoted run opened at `from` and whether it is unterminated. A doubled
/// quote is an escaped quote; with `backslash` a backslash escapes the next byte.
fn scan_quoted(b: &[u8], from: usize, quote: u8, backslash: bool) -> (usize, bool) {
    let n = b.len();
    let mut j = from + 1;
    while j < n {
        if backslash && b[j] == b'\\' {
            j += 2;
        } else if b[j] == quote {
            if b.get(j + 1) == Some(&quote) {
                j += 2;
            } else {
                return (j + 1, false);
            }
        } else {
            j += 1;
        }
    }
    (n, true)
}

fn word_end(text: &str, from: usize) -> usize {
    let mut end = from;
    if let Some(rest) = text.get(from..) {
        for (off, ch) in rest.char_indices() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                end = from + off + ch.len_utf8();
            } else {
                break;
            }
        }
    }
    end
}

/// A T-SQL word continues with alphanumerics, `_`, `$`, `#` and `@`.
fn tsql_word_end(text: &str, from: usize) -> usize {
    let mut end = from;
    if let Some(rest) = text.get(from..) {
        for (off, ch) in rest.char_indices() {
            if ch.is_alphanumeric() || matches!(ch, '_' | '$' | '#' | '@') {
                end = from + off + ch.len_utf8();
            } else {
                break;
            }
        }
    }
    end
}

/// A T-SQL `[..]` identifier: `]]` is an escaped `]`.
fn bracket_ident(b: &[u8], from: usize) -> (TokKind, usize, bool) {
    let mut j = from + 1;
    while j < b.len() {
        if b[j] == b']' {
            if b.get(j + 1) == Some(&b']') {
                j += 2;
            } else {
                return (TokKind::Quoted, j + 1, false);
            }
        } else {
            j += 1;
        }
    }
    (TokKind::Quoted, b.len(), true)
}

/// A T-SQL token that starts with `$`, `@` or `#`: money (`$5`) is a number, `$action` and a temp table (`#t`,
/// `##t`) are words, a variable (`@x`, `@@rowcount`) is an operand, anything else is punctuation.
fn tsql_sigil(text: &str, from: usize) -> (TokKind, usize, bool) {
    let b = text.as_bytes();
    let next = b.get(from + 1).copied();
    match (b[from], next) {
        (b'$', Some(d)) if d.is_ascii_digit() => (TokKind::Number, number_end(b, from + 1), false),
        (b'$', Some(d)) if d.is_ascii_alphabetic() || d == b'_' => {
            (TokKind::Word, tsql_word_end(text, from + 1), false)
        }
        (b'#', Some(d)) if d == b'#' || d == b'_' || d.is_ascii_alphanumeric() => {
            (TokKind::Word, tsql_word_end(text, from + 1), false)
        }
        (b'@', _) => {
            let mut j = from + 1;
            while b.get(j) == Some(&b'@') {
                j += 1;
            }
            (TokKind::Number, tsql_word_end(text, j).max(j), false)
        }
        _ => (TokKind::Punct, from + 1, false),
    }
}

fn number_end(b: &[u8], from: usize) -> usize {
    let mut j = from;
    while j < b.len() {
        if b[j].is_ascii_alphanumeric() || b[j] == b'_' {
            j += 1;
        } else if b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit) {
            j += 2;
        } else {
            break;
        }
    }
    j
}

/// A `$` in `PostgreSQL`: a parameter, a dollar-quoted string, or a lone character.
fn dollar(text: &str, from: usize) -> (TokKind, usize, bool) {
    let b = text.as_bytes();
    let n = b.len();
    if b.get(from + 1).is_some_and(u8::is_ascii_digit) {
        let mut j = from + 1;
        while j < n && b[j].is_ascii_digit() {
            j += 1;
        }
        return (TokKind::Number, j, false);
    }
    let mut j = from + 1;
    while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    if j < n && b[j] == b'$' {
        let delim = text.get(from..=j).unwrap_or("$");
        let body = j + 1;
        return match text.get(body..).and_then(|rest| rest.find(delim)) {
            Some(p) => (TokKind::Str, body + p + delim.len(), false),
            None => (TokKind::Str, n, true),
        };
    }
    (TokKind::Punct, from + 1, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str, d: Dialect) -> Vec<(TokKind, &str, bool)> {
        lex(text, d)
            .into_iter()
            .map(|t| (t.kind, t.text(text), t.unterminated))
            .collect()
    }

    #[test]
    fn words_numbers_punct() {
        let t = kinds("select a.b, 12.5 from t;", Dialect::Postgres);
        let texts: Vec<_> = t.iter().map(|x| x.1).collect();
        assert_eq!(
            texts,
            ["select", "a", ".", "b", ",", "12.5", "from", "t", ";"]
        );
        assert_eq!(t[5].0, TokKind::Number);
    }

    #[test]
    fn postgres_strings_and_quotes() {
        let t = kinds(r#""a""b" 'it''s' E'x\'y' $t$ a ; b $t$"#, Dialect::Postgres);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0], (TokKind::Quoted, r#""a""b""#, false));
        assert_eq!(t[1], (TokKind::Str, "'it''s'", false));
        assert_eq!(t[2], (TokKind::Str, r"E'x\'y'", false));
        assert_eq!(t[3], (TokKind::Str, "$t$ a ; b $t$", false));
    }

    #[test]
    fn postgres_params_and_nested_comments() {
        let t = kinds("$1 /* a /* b */ c */ x", Dialect::Postgres);
        assert_eq!(t[0].0, TokKind::Number);
        assert_eq!(t[1], (TokKind::Comment, "/* a /* b */ c */", false));
        assert_eq!(t[2].1, "x");
    }

    #[test]
    fn mysql_rules() {
        let t = kinds(r#"`a``b` "x\"y" 'z' # c"#, Dialect::MySql);
        assert_eq!(t[0], (TokKind::Quoted, "`a``b`", false));
        assert_eq!(t[1], (TokKind::Str, r#""x\"y""#, false));
        assert_eq!(t[2].0, TokKind::Str);
        assert_eq!(t[3], (TokKind::Comment, "# c", true));
        let t = kinds("a --b\nc -- d", Dialect::MySql);
        assert_eq!(t[1].0, TokKind::Punct);
        assert_eq!(t.last().map(|x| x.0), Some(TokKind::Comment));
    }

    #[test]
    fn sqlite_brackets() {
        let t = kinds("[my col] `x` \"y\"", Dialect::Sqlite);
        assert!(t.iter().all(|x| x.0 == TokKind::Quoted && !x.2));
        assert_eq!(t[0].1, "[my col]");
    }

    #[test]
    fn unterminated_tokens_run_to_eof() {
        for (text, kind) in [
            ("'abc", TokKind::Str),
            ("\"abc", TokKind::Quoted),
            ("/* abc", TokKind::Comment),
            ("$$ abc", TokKind::Str),
        ] {
            let t = lex(text, Dialect::Postgres);
            assert_eq!(t.len(), 1, "{text}");
            assert_eq!(
                (t[0].kind, t[0].end, t[0].unterminated),
                (kind, text.len(), true)
            );
        }
        let t = lex("[abc", Dialect::Sqlite);
        assert!(t[0].unterminated);
    }

    #[test]
    fn tsql_quoting_comments_and_variables() {
        let d = Dialect::TSql;
        let t = kinds("[a]]b] \"x\"\"y\" N'it''s' `", d);
        assert_eq!(t[0], (TokKind::Quoted, "[a]]b]", false));
        assert_eq!(t[1], (TokKind::Quoted, "\"x\"\"y\"", false));
        assert_eq!(t[2], (TokKind::Str, "N'it''s'", false));
        // A backtick is not a quote in T-SQL.
        assert_eq!(t[3].0, TokKind::Punct);
        // No backslash escapes in strings; `#` is not a comment; block comments nest.
        let t = kinds(r"'a\' #t ##g /* /* */ x */ y", d);
        assert_eq!(t[0], (TokKind::Str, r"'a\'", false));
        assert_eq!(t[1], (TokKind::Word, "#t", false));
        assert_eq!(t[2], (TokKind::Word, "##g", false));
        assert_eq!(t[3], (TokKind::Comment, "/* /* */ x */", false));
        assert_eq!(t[4], (TokKind::Word, "y", false));
        // Variables and money are operands, `$action` is a word.
        let t = kinds("@x @@rowcount $5 $action", d);
        assert_eq!(t[0], (TokKind::Number, "@x", false));
        assert_eq!(t[1], (TokKind::Number, "@@rowcount", false));
        assert_eq!(t[2], (TokKind::Number, "$5", false));
        assert_eq!(t[3], (TokKind::Word, "$action", false));
        let t = lex("[abc", d);
        assert!(t[0].unterminated && t[0].kind == TokKind::Quoted);
    }

    #[test]
    fn non_ascii_is_safe() {
        for d in Dialect::ALL {
            let text = "select é名🙂 'ü' \"名\" -- 🙂\nx";
            for t in lex(text, d) {
                assert!(text.is_char_boundary(t.start) && text.is_char_boundary(t.end));
            }
        }
        let t = kinds("é名 🙂", Dialect::Postgres);
        assert_eq!(t[0], (TokKind::Word, "é名", false));
        assert_eq!(t[1].0, TokKind::Punct);
    }
}
