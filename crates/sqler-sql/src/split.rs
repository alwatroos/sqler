// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Splitting a script into statements, and finding the statement under the cursor.

use std::ops::Range;

use crate::Dialect;
use crate::lexer::{Lexer, MAX_DELIMITER_LEN, Token, TokenKind, is_ws};

/// What a [`Segment`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SegmentKind {
    /// A statement: it has at least one token that is not whitespace or a comment.
    Statement,
    /// A `MySQL` `DELIMITER x` client line.
    Delimiter,
    /// Only whitespace and comments, maybe with a lone `;`.
    Trivia,
}

/// One piece of a script. The spans of all segments tile the text without gaps.
///
/// `body` lies inside `span`. It has no leading or trailing whitespace, keeps comments and
/// excludes the terminator. The terminator follows the body; whitespace and comments after it
/// belong to the next segment. A segment with no content has an empty body at `span.start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// What the segment holds.
    pub kind: SegmentKind,
    /// The whole piece, from the end of the previous segment to the end of the terminator.
    pub span: Range<usize>,
    /// The content, trimmed.
    pub body: Range<usize>,
    /// The `;` or custom delimiter that ended the segment, if any.
    pub terminator: Option<Range<usize>>,
}

impl Segment {
    /// The end of the statement proper: the end of the terminator, or else of the body.
    fn end(&self) -> usize {
        self.terminator.as_ref().map_or(self.body.end, |t| t.end)
    }
}

/// Splits `text` into segments whose spans tile `0..text.len()` in order.
///
/// A `;` (or the active `MySQL` custom delimiter) ends a statement unless it sits in a string,
/// quoted identifier, comment or dollar quote, in a `BEGIN ... END` body of a trigger,
/// procedure, function or event, or (on `PostgreSQL`, as in `psql`) inside parentheses. With a
/// custom delimiter active, `;` no longer splits and the delimiter always does, as in the
/// `mysql` client. A string, quoted identifier, comment or block that is not closed makes the
/// rest of the text one statement.
///
/// On `MySQL` a `DELIMITER x` line at the start of a statement yields a
/// [`SegmentKind::Delimiter`] segment and changes the delimiter (`DELIMITER ;` restores it).
/// The delimiter word is at most 16 bytes; a longer word is treated as ordinary text.
///
/// Pure and linear in the size of `text`; it never panics.
#[must_use]
pub fn split(dialect: Dialect, text: &str) -> Vec<Segment> {
    let mut lexer = Lexer::new(dialect, text);
    let mut out = Vec::new();
    let mut delimiter: Option<&str> = None;
    let mut st = State::new(0);

    while let Some(tok) = lexer.next() {
        if matches!(tok.kind, TokenKind::Whitespace) {
            continue;
        }
        if tok.is_trivia() {
            st.note(&tok);
            continue;
        }
        if dialect == Dialect::MySql
            && !st.significant
            && tok.is_keyword(text, "DELIMITER")
            && let Some((word, line_end)) = parse_delimiter(text, tok.end)
        {
            let first = st.first.unwrap_or(tok.start);
            let body_end = first + text[first..line_end].trim_end_matches(is_ws).len();
            out.push(Segment {
                kind: SegmentKind::Delimiter,
                span: st.start..line_end,
                body: first..body_end,
                terminator: None,
            });
            delimiter = if &text[word.clone()] == ";" {
                None
            } else {
                Some(&text[word])
            };
            lexer.set_delimiter(delimiter);
            lexer.seek(line_end);
            st = State::new(line_end);
            continue;
        }

        st.track(dialect, text, &tok);
        let ends = match tok.kind {
            TokenKind::Delimiter => true,
            TokenKind::Punct => {
                delimiter.is_none() && text.as_bytes()[tok.start] == b';' && st.can_split(dialect)
            }
            _ => false,
        };
        if ends {
            out.push(st.finish(text, tok.end, Some(tok.start..tok.end)));
            st = State::new(tok.end);
        } else {
            st.note(&tok);
            st.significant = true;
        }
    }
    if st.start < text.len() {
        out.push(st.finish(text, text.len(), None));
    }
    out
}

/// Parses the rest of a `DELIMITER` line: horizontal whitespace, then a word of at most
/// [`MAX_DELIMITER_LEN`] bytes. Returns the word's range and the end of the line.
fn parse_delimiter(text: &str, from: usize) -> Option<(Range<usize>, usize)> {
    let rest = &text[from..];
    let word_off = rest
        .char_indices()
        .find(|&(_, c)| c == '\r' || c == '\n' || !is_ws(c))
        .map_or(rest.len(), |(i, _)| i);
    if word_off == 0 {
        return None;
    }
    let word_start = from + word_off;
    let mut word_end = word_start;
    for (i, c) in text[word_start..].char_indices() {
        if is_ws(c) {
            break;
        }
        word_end = word_start + i + c.len_utf8();
        if word_end - word_start > MAX_DELIMITER_LEN {
            return None;
        }
    }
    if word_end == word_start {
        return None;
    }
    let line_end = text[word_end..]
        .find(['\r', '\n'])
        .map_or(text.len(), |i| word_end + i);
    // Anything after the word that is not a comment would never be classified: treat the line
    // as ordinary statement text instead.
    let tail = text[word_end..line_end].trim_matches(is_ws);
    let comment = tail.starts_with('#')
        || (tail.starts_with("--")
            && tail
                .as_bytes()
                .get(2)
                .is_none_or(|&b| b <= 0x20 || b == 0x7f));
    if !tail.is_empty() && !comment {
        return None;
    }
    Some((word_start..word_end, line_end))
}

/// Incremental detector of compound statements (see [`is_compound`]).
#[derive(Default)]
struct Compound {
    count: usize,
    create: bool,
    begin_first: bool,
}

/// The outcome of a decision: whether the statement is compound and whether a block was
/// already opened by tokens before the deciding one.
struct Decision {
    compound: bool,
    opened_block: bool,
}

impl Compound {
    /// Feeds the next significant token; `Some` once the question is settled.
    fn push(&mut self, dialect: Dialect, src: &str, tok: &Token) -> Option<Decision> {
        let no = Some(Decision {
            compound: false,
            opened_block: false,
        });
        self.count += 1;
        if self.count == 1 {
            if tok.is_keyword(src, "CREATE") {
                self.create = true;
                return None;
            }
            if dialect == Dialect::MySql && tok.is_keyword(src, "BEGIN") {
                self.begin_first = true;
                return None;
            }
            return no;
        }
        if self.begin_first {
            let want = if self.count == 2 { "NOT" } else { "ATOMIC" };
            if !tok.is_keyword(src, want) {
                return no;
            }
            return (self.count == 3).then_some(Decision {
                compound: true,
                opened_block: true,
            });
        }
        if tok.kind == TokenKind::Word {
            for kw in ["TRIGGER", "PROCEDURE", "FUNCTION", "EVENT"] {
                if tok.is_keyword(src, kw) {
                    return Some(Decision {
                        compound: true,
                        opened_block: false,
                    });
                }
            }
            if tok.is_keyword(src, "AS") || tok.is_keyword(src, "BEGIN") {
                return no;
            }
        }
        if matches!(tok.kind, TokenKind::Delimiter) {
            return no;
        }
        if tok.kind == TokenKind::Punct && matches!(src.as_bytes()[tok.start], b'(' | b';') {
            return no;
        }
        None
    }
}

/// Whether `tokens` (a statement's tokens, trivia allowed) start a trigger, procedure,
/// function or event definition, or on `MySQL` a `BEGIN NOT ATOMIC` block: the statement
/// body may then contain `;` and refers to routine arguments.
pub(crate) fn is_compound(dialect: Dialect, src: &str, tokens: &[Token]) -> bool {
    let mut det = Compound::default();
    for tok in tokens.iter().filter(|t| !t.is_trivia()) {
        if let Some(d) = det.push(dialect, src, tok) {
            return d.compound;
        }
    }
    false
}

/// The state of the statement being read.
struct State {
    start: usize,
    first: Option<usize>,
    last_end: usize,
    significant: bool,
    compound: Option<bool>,
    detector: Compound,
    block_depth: usize,
    paren_depth: usize,
    pending_end: bool,
}

impl State {
    fn new(start: usize) -> Self {
        Self {
            start,
            first: None,
            last_end: start,
            significant: false,
            compound: None,
            detector: Compound::default(),
            block_depth: 0,
            paren_depth: 0,
            pending_end: false,
        }
    }

    /// Records a non-whitespace token as part of the body.
    fn note(&mut self, tok: &Token) {
        self.first.get_or_insert(tok.start);
        self.last_end = tok.end;
    }

    fn can_split(&self, dialect: Dialect) -> bool {
        self.block_depth == 0 && (dialect != Dialect::Postgres || self.paren_depth == 0)
    }

    /// Updates compound detection, block depth and parenthesis depth for a significant token.
    fn track(&mut self, dialect: Dialect, src: &str, tok: &Token) {
        if self.compound.is_none()
            && let Some(decision) = self.detector.push(dialect, src, tok)
        {
            self.compound = Some(decision.compound);
            if decision.opened_block {
                self.block_depth = 1;
            }
        }
        if self.compound == Some(true) {
            self.track_block(src, tok);
        }
        if tok.kind == TokenKind::Punct {
            match src.as_bytes()[tok.start] {
                b'(' => self.paren_depth += 1,
                b')' => self.paren_depth = self.paren_depth.saturating_sub(1),
                _ => {}
            }
        }
    }

    /// `BEGIN` and `CASE` open a block. `END` closes one unless the next significant word is
    /// `IF`, `LOOP`, `WHILE`, `REPEAT` or `FOR`; `END CASE` closes the `CASE` once.
    fn track_block(&mut self, src: &str, tok: &Token) {
        if self.pending_end {
            self.pending_end = false;
            if ["IF", "LOOP", "WHILE", "REPEAT", "FOR"]
                .iter()
                .any(|kw| tok.is_keyword(src, kw))
            {
                return;
            }
            self.block_depth = self.block_depth.saturating_sub(1);
            if tok.is_keyword(src, "CASE") {
                return;
            }
        }
        if tok.is_keyword(src, "BEGIN") || tok.is_keyword(src, "CASE") {
            self.block_depth += 1;
        } else if tok.is_keyword(src, "END") {
            self.pending_end = true;
        }
    }

    fn finish(&self, src: &str, end: usize, terminator: Option<Range<usize>>) -> Segment {
        let body = match self.first {
            Some(first) => {
                // An unterminated token runs to the end and may carry trailing whitespace.
                let trimmed = src[first..self.last_end].trim_end_matches(is_ws);
                first..first + trimmed.len()
            }
            None => self.start..self.start,
        };
        Segment {
            kind: if self.significant {
                SegmentKind::Statement
            } else {
                SegmentKind::Trivia
            },
            span: self.start..end,
            body,
            terminator,
        }
    }
}

/// The statement under the cursor.
///
/// Only [`SegmentKind::Statement`] segments count. `offset` is clamped to the end of the last
/// segment. In order, the result is:
///
/// 1. the statement with `body.start <= offset < end`, where `end` is the end of the
///    terminator or else of the body;
/// 2. otherwise the statement with the largest `end <= offset` (the cursor sits on whitespace
///    or comments after it);
/// 3. otherwise the first statement.
///
/// `None` when there is no statement.
#[must_use]
pub fn statement_at(segments: &[Segment], offset: usize) -> Option<&Segment> {
    let limit = segments.last()?.span.end;
    let offset = offset.min(limit);
    let mut first = None;
    let mut before: Option<&Segment> = None;
    for seg in segments.iter().filter(|s| s.kind == SegmentKind::Statement) {
        first.get_or_insert(seg);
        let end = seg.end();
        if seg.body.start <= offset && offset < end {
            return Some(seg);
        }
        if end <= offset && before.is_none_or(|b| end >= b.end()) {
            before = Some(seg);
        }
    }
    before.or(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parts(d: Dialect, s: &str) -> Vec<(SegmentKind, &str, Option<&str>)> {
        split(d, s)
            .into_iter()
            .map(|g| (g.kind, &s[g.body.clone()], g.terminator.map(|t| &s[t])))
            .collect()
    }

    #[test]
    fn empty_input_has_no_segments() {
        for d in Dialect::ALL {
            assert_eq!(split(d, "").len(), 0);
        }
    }

    #[test]
    fn basic_layout() {
        let s = " select 1 ; -- c\n select 2";
        let g = split(Dialect::Postgres, s);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].span, 0..11);
        assert_eq!(g[0].body, 1..9);
        assert_eq!(g[0].terminator, Some(10..11));
        assert_eq!(g[1].span, 11..s.len());
        assert_eq!(&s[g[1].body.clone()], "-- c\n select 2");
        assert_eq!(g[1].terminator, None);
    }

    #[test]
    fn trivia_segments() {
        let g = parts(Dialect::Sqlite, "select 1; -- tail\n");
        assert_eq!(g[1], (SegmentKind::Trivia, "-- tail", None));
        let g = parts(Dialect::Sqlite, ";");
        assert_eq!(g, vec![(SegmentKind::Trivia, "", Some(";"))]);
        let g = parts(Dialect::Sqlite, "  \n");
        assert_eq!(g, vec![(SegmentKind::Trivia, "", None)]);
    }

    #[test]
    fn compound_detection() {
        let case = |d, s: &str, want| {
            let t = tokenize(d, s);
            assert_eq!(is_compound(d, s, &t), want, "{s}");
        };
        case(
            Dialect::Sqlite,
            "create trigger t after insert on x begin select 1; end",
            true,
        );
        case(
            Dialect::MySql,
            "CREATE DEFINER=`r`@`h` PROCEDURE p() BEGIN END",
            true,
        );
        case(
            Dialect::Postgres,
            "create or replace function f() returns int as $$ $$",
            true,
        );
        case(
            Dialect::MySql,
            "create event e on schedule every 1 day do select 1",
            true,
        );
        case(Dialect::MySql, "begin not atomic select 1; end", true);
        case(Dialect::Postgres, "begin not atomic select 1; end", false);
        case(Dialect::MySql, "begin", false);
        case(Dialect::MySql, "begin work", false);
        case(Dialect::MySql, "begin not", false);
        case(Dialect::Postgres, "create table t (trigger int)", false);
        case(
            Dialect::Postgres,
            "create view v as select 1 function",
            false,
        );
        case(Dialect::Postgres, "create", false);
        case(Dialect::Postgres, "select 1", false);
        case(Dialect::Postgres, "", false);
    }

    #[test]
    fn delimiter_line_parsing() {
        assert_eq!(parse_delimiter("DELIMITER //\nx", 9), Some((10..12, 12)));
        assert!(parse_delimiter("DELIMITER\n//", 9).is_none());
        assert!(parse_delimiter("DELIMITER", 9).is_none());
        assert!(parse_delimiter("DELIMITER x", 9).is_some());
        assert!(parse_delimiter("DELIMITERx", 9).is_none());
        let long = format!("DELIMITER {}", "x".repeat(MAX_DELIMITER_LEN + 1));
        assert!(parse_delimiter(&long, 9).is_none());
        let ok = format!("DELIMITER {}", "x".repeat(MAX_DELIMITER_LEN));
        assert!(parse_delimiter(&ok, 9).is_some());
    }
}
