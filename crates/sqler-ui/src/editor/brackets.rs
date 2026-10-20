// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Bracket matching: `()`, `[]` and `{}` outside strings, quoted identifiers and comments.
//!
//! The scanner mirrors the rules of the `sqler-sql` lexer (which is private): `'...'` and `"..."`, `--` and
//! `/* */` comments; `PostgreSQL` also has `$tag$...$tag$` and nested block comments; `MySQL` also has
//! `` `...` `` and `#` comments, and reads a backslash in a string as an escape. Unmatched brackets are ignored.

use sqler_sql::Dialect;

fn is_bracket(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}')
}

fn closer(open: u8) -> u8 {
    match open {
        b'(' => b')',
        b'[' => b']',
        _ => b'}',
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Whether the char at or just before `cursor` is a bracket (`()[]{}`).
#[must_use]
pub fn touches_bracket(text: &str, cursor: usize) -> bool {
    let bytes = text.as_bytes();
    bytes.get(cursor).copied().is_some_and(is_bracket)
        || cursor
            .checked_sub(1)
            .and_then(|p| bytes.get(p))
            .copied()
            .is_some_and(is_bracket)
}

/// The matched bracket pairs of a text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BracketIndex {
    /// `(open, close)` sorted by `open`.
    by_open: Vec<(usize, usize)>,
    /// The same pairs sorted by `close`.
    by_close: Vec<(usize, usize)>,
}

/// Where a quoted construct that starts at `i` ends (exclusive), or `i` if there is none.
struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    dialect: Option<Dialect>,
}

impl Scanner<'_> {
    fn mysql(&self) -> bool {
        self.dialect == Some(Dialect::MySql)
    }

    fn postgres(&self) -> bool {
        self.dialect == Some(Dialect::Postgres)
    }

    /// The end of the quoted run that starts at `i` with `quote`.
    fn quoted_end(&self, i: usize, quote: u8, escapes: bool) -> usize {
        let mut j = i + 1;
        while let Some(&b) = self.bytes.get(j) {
            if escapes && b == b'\\' {
                j += 2;
            } else if b == quote {
                return j + 1;
            } else {
                j += 1;
            }
        }
        self.bytes.len()
    }

    fn line_end(&self, i: usize) -> usize {
        self.bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(self.bytes.len(), |p| i + p)
    }

    fn block_comment_end(&self, i: usize) -> usize {
        let mut depth = 1usize;
        let mut j = i + 2;
        while j < self.bytes.len() {
            match (self.bytes[j], self.bytes.get(j + 1)) {
                (b'*', Some(b'/')) => {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        return j;
                    }
                }
                (b'/', Some(b'*')) if self.postgres() => {
                    depth += 1;
                    j += 2;
                }
                _ => j += 1,
            }
        }
        self.bytes.len()
    }

    /// The end of a dollar-quoted string starting at `i`, if one starts there.
    fn dollar_end(&self, i: usize) -> Option<usize> {
        if i > 0 && is_ident_byte(self.bytes[i - 1]) {
            return None;
        }
        let rest = &self.bytes[i + 1..];
        let tag_len = match rest.first() {
            Some(b'$') => 0,
            Some(&b) if b.is_ascii_alphabetic() || b == b'_' => {
                let n = rest
                    .iter()
                    .take_while(|&&c| c.is_ascii_alphanumeric() || c == b'_')
                    .count();
                if rest.get(n) != Some(&b'$') {
                    return None;
                }
                n
            }
            _ => return None,
        };
        let delim = &self.text[i..i + tag_len + 2];
        let body = i + tag_len + 2;
        Some(
            self.text[body..]
                .find(delim)
                .map_or(self.bytes.len(), |p| body + p + delim.len()),
        )
    }

    /// Whether the `'` at `i` opens an `E'...'` string (backslash escapes).
    fn escape_string(&self, i: usize) -> bool {
        self.mysql()
            || (self.postgres()
                && i > 0
                && matches!(self.bytes[i - 1], b'e' | b'E')
                && (i < 2 || !is_ident_byte(self.bytes[i - 2])))
    }
}

impl BracketIndex {
    /// Scans `text` for matched bracket pairs. `dialect` selects the quoting and comment rules (`None`: the common
    /// ones).
    #[must_use]
    pub fn build(d: Option<Dialect>, text: &str) -> Self {
        let sc = Scanner {
            text,
            bytes: text.as_bytes(),
            dialect: d,
        };
        let bytes = sc.bytes;
        let mut stack: Vec<(u8, usize)> = Vec::new();
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            let next = bytes.get(i + 1).copied();
            i = match b {
                b'\'' => sc.quoted_end(i, b'\'', sc.escape_string(i)),
                b'"' => sc.quoted_end(i, b'"', sc.mysql()),
                b'`' if sc.mysql() => sc.quoted_end(i, b'`', false),
                b'-' if next == Some(b'-') => sc.line_end(i),
                b'#' if sc.mysql() => sc.line_end(i),
                b'/' if next == Some(b'*') => sc.block_comment_end(i),
                b'$' if sc.postgres() => sc.dollar_end(i).unwrap_or(i + 1),
                b'(' | b'[' | b'{' => {
                    stack.push((b, i));
                    i + 1
                }
                b')' | b']' | b'}' => {
                    if let Some(&(open, at)) = stack.last()
                        && closer(open) == b
                    {
                        stack.pop();
                        pairs.push((at, i));
                    }
                    i + 1
                }
                _ => i + 1,
            };
        }
        pairs.sort_unstable();
        let mut by_close = pairs.clone();
        by_close.sort_unstable_by_key(|&(_, close)| close);
        Self {
            by_open: pairs,
            by_close,
        }
    }

    /// The pair for the bracket at `cursor`, else for the one just before it. `text` must be the text the index
    /// was built from.
    #[must_use]
    pub fn pair_at(&self, text: &str, cursor: usize) -> Option<(usize, usize)> {
        let bytes = text.as_bytes();
        let at = |p: usize| {
            bytes
                .get(p)
                .copied()
                .filter(|&b| is_bracket(b))
                .and_then(|_| self.find(p))
        };
        at(cursor).or_else(|| cursor.checked_sub(1).and_then(at))
    }

    fn find(&self, p: usize) -> Option<(usize, usize)> {
        if let Ok(i) = self.by_open.binary_search_by_key(&p, |&(open, _)| open) {
            return self.by_open.get(i).copied();
        }
        let i = self
            .by_close
            .binary_search_by_key(&p, |&(_, close)| close)
            .ok()?;
        self.by_close.get(i).copied()
    }

    /// How many pairs there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_open.len()
    }

    /// Whether there is no pair.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_open.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(d: Option<Dialect>, t: &str) -> Vec<(usize, usize)> {
        BracketIndex::build(d, t).by_open
    }

    #[test]
    fn nested_pairs() {
        assert_eq!(pairs(None, "f(a[1], {b})"), vec![(1, 11), (3, 5), (8, 10)]);
    }

    #[test]
    fn pair_at_checks_at_and_before_the_cursor() {
        let t = "(a(b))";
        let idx = BracketIndex::build(None, t);
        assert_eq!(idx.pair_at(t, 0), Some((0, 5)), "at the opening");
        assert_eq!(idx.pair_at(t, 1), Some((0, 5)), "after the opening");
        assert_eq!(idx.pair_at(t, 2), Some((2, 4)), "at the inner opening");
        assert_eq!(idx.pair_at(t, 5), Some((0, 5)), "at the closing");
        assert_eq!(idx.pair_at(t, 6), Some((0, 5)), "after the closing");
        assert_eq!(idx.pair_at(t, 3), Some((2, 4)), "after the inner opening");
    }

    #[test]
    fn at_the_cursor_wins_over_before() {
        let t = ")(x)";
        let idx = BracketIndex::build(None, t);
        // The first `)` is unmatched and ignored.
        assert_eq!(idx.pair_at(t, 1), Some((1, 3)));
        let t = "(a)(b)";
        let idx = BracketIndex::build(None, t);
        assert_eq!(idx.pair_at(t, 3), Some((3, 5)), "the `(` at the cursor");
    }

    #[test]
    fn no_pair_away_from_brackets() {
        let t = "(a) b";
        let idx = BracketIndex::build(None, t);
        assert_eq!(idx.pair_at(t, 4), None);
        assert_eq!(idx.pair_at(t, 99), None);
        assert!(!touches_bracket(t, 4));
    }

    #[test]
    fn touches_bracket_before_and_after() {
        assert!(touches_bracket("a(b", 1));
        assert!(touches_bracket("a(b", 2));
        assert!(!touches_bracket("a(b", 0));
        assert!(!touches_bracket("a(b", 3));
        assert!(!touches_bracket("", 0));
    }

    #[test]
    fn strings_and_quoted_identifiers_hide_brackets() {
        assert_eq!(pairs(None, "f('(' , \")\")"), vec![(1, 11)]);
        assert_eq!(pairs(None, "('it''s (' )"), vec![(0, 11)]);
        assert_eq!(pairs(Some(Dialect::Postgres), "(\"a(\")"), vec![(0, 5)]);
    }

    #[test]
    fn comments_hide_brackets() {
        assert_eq!(pairs(None, "( -- )\n)"), vec![(0, 7)]);
        assert_eq!(pairs(None, "( /* ) */ )"), vec![(0, 10)]);
        assert_eq!(
            pairs(None, "( /* unterminated )"),
            Vec::<(usize, usize)>::new()
        );
    }

    #[test]
    fn postgres_nests_block_comments_others_do_not() {
        let t = "( /* a /* ) */ ) */ )";
        assert_eq!(pairs(Some(Dialect::Postgres), t), vec![(0, 20)]);
        assert_eq!(pairs(Some(Dialect::Sqlite), t), vec![(0, 15)]);
    }

    #[test]
    fn dollar_quotes_in_postgres() {
        let pg = Some(Dialect::Postgres);
        assert_eq!(pairs(pg, "( $$ ) $$ )"), vec![(0, 10)]);
        assert_eq!(pairs(pg, "( $fn$ ) $$ ) $fn$ )"), vec![(0, 19)]);
        // Not a dollar quote: positional parameter, and `$` inside an identifier.
        assert_eq!(pairs(pg, "(a = $1)"), vec![(0, 7)]);
        assert_eq!(pairs(pg, "(a$b$ )"), vec![(0, 6)]);
        // Other dialects do not know them.
        assert_eq!(pairs(Some(Dialect::Sqlite), "( $$ ) $$ )"), vec![(0, 5)]);
        assert_eq!(pairs(pg, "( $$ )"), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn mysql_backticks_hash_comments_and_escapes() {
        let my = Some(Dialect::MySql);
        assert_eq!(pairs(my, "(`a)` )"), vec![(0, 6)]);
        assert_eq!(pairs(None, "(`a)` )"), vec![(0, 3)]);
        assert_eq!(pairs(my, "( # )\n)"), vec![(0, 6)]);
        assert_eq!(pairs(None, "( # )\n)"), vec![(0, 4)]);
        assert_eq!(pairs(my, r"('\')' )"), vec![(0, 7)]);
        assert_eq!(pairs(Some(Dialect::Sqlite), r"('\')' )"), vec![(0, 4)]);
    }

    #[test]
    fn postgres_escape_strings() {
        let pg = Some(Dialect::Postgres);
        assert_eq!(pairs(pg, r"(E'\')' )"), vec![(0, 8)]);
        assert_eq!(pairs(pg, r"('\')"), vec![(0, 4)]);
    }

    #[test]
    fn mismatched_and_unmatched_are_ignored() {
        assert_eq!(pairs(None, "( ]"), Vec::<(usize, usize)>::new());
        assert_eq!(pairs(None, "( ] )"), vec![(0, 4)]);
        assert_eq!(pairs(None, ") ("), Vec::<(usize, usize)>::new());
        assert_eq!(pairs(None, "[(])"), vec![(1, 3)]);
    }

    #[test]
    fn multibyte_text() {
        let t = "('żółć') x";
        let idx = BracketIndex::build(None, t);
        assert_eq!(idx.pair_at(t, 0), Some((0, 11)));
        assert_eq!(idx.pair_at(t, 3), None, "inside a char");
    }
}
