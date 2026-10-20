// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! SQL formatting: keyword case and whitespace between tokens, nothing else.
//!
//! The formatter works on the tokens of the lexer, never on a syntax tree. It changes only the
//! case of a fixed list of keywords and the whitespace between tokens. Strings, quoted
//! identifiers, dollar-quoted bodies, comments, `MySQL` executable comments and the bodies of
//! compound statements (triggers, procedures, functions, events) are copied byte for byte.

use crate::lexer::{Token, TokenKind, tokenize};
use crate::split::{Segment, SegmentKind, is_compound, split};
use crate::{Dialect, FormatError};

/// The most indent levels that are drawn; deeper nesting stays at this column.
const MAX_DEPTH: usize = 16;
/// The widest indent step that is drawn, in spaces.
const MAX_WIDTH: usize = 8;

/// Keywords that are written in upper case. Each is a reserved word in `MySQL` 8.4
/// (checked against the manual on 2026-10-20).
const UPPERCASE: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "IS", "NULL", "AS", "ON", "JOIN", "INNER",
    "LEFT", "RIGHT", "CROSS", "NATURAL", "OUTER", "USING", "GROUP", "BY", "ORDER", "HAVING",
    "LIMIT", "UNION", "ALL", "DISTINCT", "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE",
    "CREATE", "TABLE", "DROP", "ALTER", "CASE", "WHEN", "THEN", "ELSE", "WITH", "EXISTS",
    "BETWEEN", "LIKE",
];

/// Keywords that start a new line.
const CLAUSE: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "VALUES",
    "SET",
    "RETURNING",
];

/// Join prefixes: they start a new line when `JOIN` or `OUTER` follows.
const JOIN_PREFIX: &[&str] = &["INNER", "LEFT", "RIGHT", "FULL", "CROSS", "NATURAL"];

/// Options for [`format()`].
///
/// Indentation is drawn at most 16 levels deep and at most 8 spaces per level, so the output
/// stays linear in the size of the input for any nesting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormatOptions {
    /// Spaces per indent level (default 2).
    pub indent_width: u8,
    /// Whether keywords are written in upper case (default `true`).
    pub uppercase_keywords: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_width: 2,
            uppercase_keywords: true,
        }
    }
}

/// Formats `text`.
///
/// Only the case of common keywords and the whitespace between tokens change; every token keeps
/// its text, so the result means the same as the input. A statement is laid out with its clauses
/// on separate lines, subqueries indented and `AND`/`OR` on their own lines. Statements are
/// separated by a blank line and the result ends with one `\n` (blank input gives `""`).
/// Strings, quoted identifiers, dollar-quoted bodies, comments, compound statements
/// (`CREATE TRIGGER/PROCEDURE/FUNCTION/EVENT`, `BEGIN NOT ATOMIC`) and `MySQL` `DELIMITER`
/// lines are copied byte for byte.
///
/// Formatting is deterministic and idempotent: formatting the output again returns it
/// unchanged. Pure and linear in the size of `text`; it never panics.
///
/// # Errors
///
/// [`FormatError::Unterminated`] if a string, quoted identifier or comment is not closed.
pub fn format(
    dialect: Dialect,
    text: &str,
    options: &FormatOptions,
) -> Result<String, FormatError> {
    let mut parts: Vec<String> = Vec::new();
    for segment in split(dialect, text) {
        let part = match segment.kind {
            SegmentKind::Statement => statement(dialect, text, &segment, options)?,
            SegmentKind::Trivia => trivia(dialect, text, &segment)?,
            _ => slice(text, segment.body.clone()).to_owned(),
        };
        if !part.is_empty() {
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return Ok(String::new());
    }
    let mut out = parts.join("\n\n");
    out.push('\n');
    Ok(out)
}

fn slice(text: &str, range: std::ops::Range<usize>) -> &str {
    text.get(range).unwrap_or("")
}

/// The first unterminated token of a segment body, as an offset into the whole text.
fn unterminated(tokens: &[Token], body_start: usize) -> Result<(), FormatError> {
    match tokens.iter().find(|t| !t.is_terminated()) {
        Some(t) => Err(FormatError::Unterminated {
            offset: body_start + t.start,
        }),
        None => Ok(()),
    }
}

/// The last token that is not whitespace.
fn last_token(tokens: &[Token]) -> Option<&Token> {
    tokens
        .iter()
        .rev()
        .find(|t| t.kind != TokenKind::Whitespace)
}

/// Appends the terminator of `segment`. A line comment before it forces a new line; a custom
/// delimiter keeps the input gap so that no token can merge with it. After an operator that ends
/// in `-` the whitespace is copied: it can be what stops a `MySQL` `--` from being a comment.
fn push_terminator(out: &mut String, text: &str, segment: &Segment, tokens: &[Token], src: &str) {
    let last = last_token(tokens);
    let dash = last.is_some_and(|t| t.kind == TokenKind::Op && t.text(src).ends_with('-'));
    let gap_end = segment
        .terminator
        .as_ref()
        .map_or(segment.span.end, |t| t.start);
    let gap = slice(text, segment.body.end..gap_end);
    let Some(term) = &segment.terminator else {
        if dash && gap.starts_with('\u{FEFF}') {
            out.push('\u{FEFF}');
        }
        return;
    };
    let term_text = slice(text, term.clone());
    if last.is_some_and(|t| t.kind == TokenKind::LineComment) {
        out.push('\n');
    } else if dash {
        out.push_str(gap);
    } else if term_text != ";" && !gap.is_empty() {
        out.push(' ');
    }
    out.push_str(term_text);
}

/// The tokens of a statement body, as the lexer sees them in the text: a trailing `--` followed
/// by the terminator is not a `MySQL` comment. Falls back to the body alone when a token reaches
/// past the body (an unterminated one, or an operator that runs into a custom delimiter).
fn body_tokens(dialect: Dialect, text: &str, segment: &Segment) -> Vec<Token> {
    let body = slice(text, segment.body.clone());
    let end = segment
        .terminator
        .as_ref()
        .map_or(segment.span.end, |t| t.end);
    let context = slice(text, segment.body.start..end);
    let kept: Vec<Token> = tokenize(dialect, context)
        .into_iter()
        .take_while(|t| t.end <= body.len())
        .collect();
    if kept.last().map_or(0, |t| t.end) == body.len() {
        kept
    } else {
        tokenize(dialect, body)
    }
}

fn trivia(dialect: Dialect, text: &str, segment: &Segment) -> Result<String, FormatError> {
    let body = slice(text, segment.body.clone());
    let tokens = tokenize(dialect, body);
    unterminated(&tokens, segment.body.start)?;
    let mut out = String::new();
    for token in tokens.iter().filter(|t| t.kind != TokenKind::Whitespace) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(token.text(body));
    }
    if let Some(term) = &segment.terminator {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(slice(text, term.clone()));
    }
    Ok(out)
}

fn statement(
    dialect: Dialect,
    text: &str,
    segment: &Segment,
    options: &FormatOptions,
) -> Result<String, FormatError> {
    let body = slice(text, segment.body.clone());
    let tokens = body_tokens(dialect, text, segment);
    unterminated(&tokens, segment.body.start)?;
    let verbatim = is_compound(dialect, body, &tokens)
        || (dialect == Dialect::MySql
            && tokens
                .iter()
                .find(|t| !t.is_trivia())
                .is_some_and(|t| t.is_keyword(body, "DELIMITER")));
    let mut out = if verbatim {
        body.to_owned()
    } else {
        layout(body, &tokens, options)
    };
    push_terminator(&mut out, text, segment, &tokens, body);
    Ok(out)
}

/// A token that is kept in the layout: not whitespace.
struct Item {
    kind: TokenKind,
    start: usize,
    end: usize,
    /// Whether whitespace separated this token from the previous one in the input.
    gap: bool,
}

impl Item {
    fn is_comment(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::LineComment | TokenKind::BlockComment { .. }
        )
    }

    fn is_punct(&self, src: &str, ch: u8) -> bool {
        self.kind == TokenKind::Punct && src.as_bytes().get(self.start) == Some(&ch)
    }
}

/// One paren level: a plain one (call, list) or a subquery or the top level.
struct Frame {
    plain: bool,
    between: bool,
}

fn is_in(list: &[&str], word: &str) -> bool {
    list.iter().any(|k| k.eq_ignore_ascii_case(word))
}

#[allow(clippy::too_many_lines)] // one pass over the items; splitting it would only pass state around
fn layout(src: &str, tokens: &[Token], options: &FormatOptions) -> String {
    let mut items: Vec<Item> = Vec::new();
    let mut gap = false;
    for t in tokens {
        if t.kind == TokenKind::Whitespace {
            gap = true;
        } else {
            items.push(Item {
                kind: t.kind,
                start: t.start,
                end: t.end,
                gap,
            });
            gap = false;
        }
    }
    let n = items.len();
    let text_of = |i: usize| src.get(items[i].start..items[i].end).unwrap_or("");

    // Neighbouring significant (non-comment) items.
    let mut prev_sig: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut last = None;
    for (i, item) in items.iter().enumerate() {
        prev_sig.push(last);
        if !item.is_comment() {
            last = Some(i);
        }
    }
    let mut next_sig: Vec<Option<usize>> = vec![None; n];
    let mut next = None;
    for i in (0..n).rev() {
        next_sig[i] = next;
        if !items[i].is_comment() {
            next = Some(i);
        }
    }

    // Keyword flags. A word next to a `.` is a name, never a keyword.
    let word_at = |i: usize| -> Option<&str> {
        if items[i].kind != TokenKind::Word {
            return None;
        }
        let dot = |j: Option<usize>| j.is_some_and(|j| items[j].is_punct(src, b'.'));
        if dot(prev_sig[i]) || dot(next_sig[i]) {
            return None;
        }
        Some(text_of(i))
    };
    let upper_kw: Vec<bool> = (0..n)
        .map(|i| word_at(i).is_some_and(|w| is_in(UPPERCASE, w)))
        .collect();
    let is_word = |j: Option<usize>, name: &str| {
        j.and_then(word_at)
            .is_some_and(|w| w.eq_ignore_ascii_case(name))
    };

    let width = usize::from(options.indent_width).min(MAX_WIDTH);
    let indent = |depth: usize| depth.min(MAX_DEPTH) * width;

    let mut out = String::with_capacity(src.len() + src.len() / 4);
    let mut frames = vec![Frame {
        plain: false,
        between: false,
    }];
    let mut depth = 0usize;
    let mut pending: Option<usize> = None;
    let mut seen_sig = false;

    for i in 0..n {
        let item = &items[i];
        let word = word_at(i);
        let mut brk: Option<usize> = None;

        if item.is_punct(src, b')') && frames.len() > 1 {
            if let Some(frame) = frames.pop()
                && !frame.plain
            {
                depth = depth.saturating_sub(1);
                brk = Some(indent(depth));
            }
        } else if let Some(w) = word {
            let top_plain = frames.last().is_some_and(|f| f.plain);
            let upper = w.to_ascii_uppercase();
            let mut skip_and = false;
            if let Some(frame) = frames.last_mut() {
                if upper == "BETWEEN" {
                    frame.between = true;
                } else if frame.between && (upper == "AND" || upper == "OR") {
                    skip_and = true;
                    if upper == "AND" {
                        frame.between = false;
                    }
                }
            }
            if seen_sig && !top_plain {
                let prev = prev_sig[i];
                let next_is_join = is_word(next_sig[i], "JOIN") || is_word(next_sig[i], "OUTER");
                if is_in(CLAUSE, &upper) {
                    brk = Some(indent(depth));
                } else if is_in(JOIN_PREFIX, &upper) {
                    if next_is_join {
                        brk = Some(indent(depth));
                    }
                } else if upper == "JOIN" {
                    let after_prefix = prev
                        .and_then(word_at)
                        .is_some_and(|p| is_in(JOIN_PREFIX, p) || p.eq_ignore_ascii_case("OUTER"));
                    if !after_prefix {
                        brk = Some(indent(depth));
                    }
                } else if (upper == "AND" || upper == "OR") && !skip_and {
                    brk = Some(indent(depth) + width);
                }
            }
        }

        let prev_item = i.checked_sub(1).map(|p| &items[p]);
        // Whitespace after `-` can turn a `MySQL` `--` into a comment, so it is copied as is.
        let keep_gap = prev_item.is_some_and(|p| ends_in_dash(src, p));
        if keep_gap {
            brk = None;
            pending = None;
        }
        if brk.is_none() {
            brk = pending;
        }
        if brk.is_none()
            && let Some(p) = prev_item
            && p.kind == TokenKind::LineComment
        {
            brk = Some(indent(depth));
        }
        pending = None;

        if let Some(p) = prev_item {
            if keep_gap {
                out.push_str(slice(src, p.end..item.start));
            } else if let Some(cols) = brk {
                out.push('\n');
                out.extend(std::iter::repeat_n(' ', cols));
            } else if space_between(src, &items, &upper_kw, i - 1, i, p) {
                out.push(' ');
            }
        }

        let raw = text_of(i);
        if options.uppercase_keywords && upper_kw[i] {
            out.push_str(&raw.to_ascii_uppercase());
        } else {
            out.push_str(raw);
        }

        if item.is_punct(src, b'(') {
            let subquery = is_word(next_sig[i], "SELECT") || is_word(next_sig[i], "WITH");
            frames.push(Frame {
                plain: !subquery,
                between: false,
            });
            if subquery {
                depth += 1;
                pending = Some(indent(depth));
            }
        }
        if !item.is_comment() {
            seen_sig = true;
        }
    }
    out
}

/// An operator that ends in `-`.
fn ends_in_dash(src: &str, item: &Item) -> bool {
    item.kind == TokenKind::Op && src.get(..item.end).is_some_and(|s| s.ends_with('-'))
}

/// Whether one space (rather than nothing) goes between item `a` (index `ai`) and item `ai + 1`.
fn space_between(
    src: &str,
    items: &[Item],
    upper_kw: &[bool],
    ai: usize,
    bi: usize,
    a: &Item,
) -> bool {
    let b = &items[bi];
    if a.is_comment() || b.is_comment() {
        return b.gap;
    }
    if b.is_punct(src, b',') || b.is_punct(src, b';') || b.is_punct(src, b')') {
        return false;
    }
    if a.is_punct(src, b'(') {
        return false;
    }
    if a.is_punct(src, b',') {
        return true;
    }
    // Servers read non-ASCII chars as identifier chars: a glued `Other` token must stay glued.
    if a.kind == TokenKind::Other || b.kind == TokenKind::Other {
        return b.gap;
    }
    if (upper_kw[ai] || upper_kw[bi]) && !b.is_punct(src, b'(') {
        return true;
    }
    b.gap
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const FRAGMENTS: &[&str] = &[
        ";",
        "'",
        "\"",
        "`",
        "$$",
        "$a$",
        "$1",
        "?",
        ":n",
        "--",
        "-- c\n",
        "#",
        "/*",
        "*/",
        "/*!5 x*/",
        "(",
        ")",
        "[",
        "]",
        ",",
        ".",
        "BEGIN",
        "END",
        "create trigger t ",
        "DELIMITER //\n",
        "//",
        "\\",
        "é",
        "\0",
        "\u{FEFF}",
        "::",
        "select",
        "SELECT",
        "from",
        "where",
        "and",
        "between",
        "join",
        "left",
        "outer",
        "order",
        "by",
        "union",
        "values",
        "set",
        "t",
        "1",
        "+",
        "-",
        "=",
        " ",
        "\n",
        "\t",
    ];

    fn any_sql() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            prop::collection::vec(prop::sample::select(FRAGMENTS), 0..40).prop_map(|v| v.concat()),
        ]
    }

    /// The token texts of every segment, whitespace left out and words lower-cased. A line
    /// comment at the very end loses its trailing whitespace, like the rest of the text.
    /// Tokens are read per segment because a `MySQL` `DELIMITER` changes how text is lexed.
    fn significant(dialect: Dialect, text: &str) -> Vec<(bool, String)> {
        let mut out = Vec::new();
        for segment in split(dialect, text) {
            let body = slice(text, segment.body.clone());
            if segment.kind == SegmentKind::Delimiter {
                out.push((false, body.to_owned()));
                continue;
            }
            for t in body_tokens(dialect, text, &segment)
                .iter()
                .filter(|t| t.kind != TokenKind::Whitespace)
            {
                let word = t.kind == TokenKind::Word;
                let s = t.text(body);
                let s = if t.kind == TokenKind::LineComment {
                    s.trim_end_matches(crate::lexer::is_ws)
                } else {
                    s
                };
                out.push((
                    word,
                    if word {
                        s.to_ascii_lowercase()
                    } else {
                        s.to_owned()
                    },
                ));
            }
            if let Some(term) = &segment.terminator {
                out.push((false, slice(text, term.clone()).to_owned()));
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1000))]

        #[test]
        fn tokens_are_unchanged(text in any_sql(), upper in any::<bool>()) {
            let options = FormatOptions { uppercase_keywords: upper, ..FormatOptions::default() };
            for dialect in Dialect::ALL {
                if let Ok(out) = format(dialect, &text, &options) {
                    prop_assert_eq!(
                        significant(dialect, &out),
                        significant(dialect, &text),
                        "{:?} {:?} -> {:?}", dialect, text, out
                    );
                }
            }
        }
    }

    #[test]
    fn keywords_only_change_case() {
        let out = format(
            Dialect::Postgres,
            "select a from t",
            &FormatOptions::default(),
        );
        assert_eq!(out, Ok("SELECT a\nFROM t\n".to_owned()));
        let keep = FormatOptions {
            uppercase_keywords: false,
            ..FormatOptions::default()
        };
        let out = format(Dialect::Postgres, "Select a From t", &keep);
        assert_eq!(out, Ok("Select a\nFrom t\n".to_owned()));
    }

    #[test]
    fn names_next_to_a_dot_keep_their_case() {
        let out = format(
            Dialect::MySql,
            "select t.order from db.from",
            &FormatOptions::default(),
        );
        assert_eq!(out, Ok("SELECT t.order\nFROM db.from\n".to_owned()));
    }

    #[test]
    fn a_line_comment_always_ends_its_line() {
        for dialect in Dialect::ALL {
            let out = format(
                dialect,
                "select 1 -- c\n, 2 -- d\n;",
                &FormatOptions::default(),
            );
            assert_eq!(
                out,
                Ok("SELECT 1 -- c\n, 2 -- d\n;\n".to_owned()),
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn an_open_quoted_variable_is_unterminated() {
        for text in ["select @'a", "select @\"a\\\"", "select @`a``"] {
            assert!(
                matches!(
                    format(Dialect::MySql, text, &FormatOptions::default()),
                    Err(FormatError::Unterminated { offset: 7 })
                ),
                "{text}"
            );
        }
        for text in ["select @'a''b'", "select @`a` , @@x.y"] {
            assert!(
                format(Dialect::MySql, text, &FormatOptions::default()).is_ok(),
                "{text}"
            );
        }
    }

    #[test]
    fn between_keeps_its_and() {
        let out = format(
            Dialect::Sqlite,
            "select 1 where a between 1 and 2 and b or c",
            &FormatOptions::default(),
        );
        assert_eq!(
            out,
            Ok("SELECT 1\nWHERE a BETWEEN 1 AND 2\n  AND b\n  OR c\n".to_owned())
        );
    }

    #[test]
    fn a_double_dash_is_not_turned_into_a_comment() {
        // On MySQL `--select` is two minus signs and a word, not a comment.
        let out = format(
            Dialect::MySql,
            "select 1--select",
            &FormatOptions::default(),
        );
        assert_eq!(out, Ok("SELECT 1--SELECT\n".to_owned()));
        let out = format(
            Dialect::MySql,
            "x --\u{FEFF}and y",
            &FormatOptions::default(),
        );
        assert!(matches!(&out, Ok(o) if o.contains("--\u{FEFF}")), "{out:?}");
    }
}
