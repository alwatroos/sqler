// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Parameter placeholders in a script.

use std::ops::Range;

use crate::Dialect;
use crate::lexer::{TokenKind, tokenize};
use crate::split::{SegmentKind, is_compound, split};

/// How a placeholder is written.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaceholderKind {
    /// `$n` (`PostgreSQL`) or `?n` (`SQLite`), numbered from 1.
    Positional(u32),
    /// `?`.
    Anonymous,
    /// A named placeholder with its sigil, such as `:id`, `@id` or `$id`.
    Named(String),
}

/// A placeholder and where it sits in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// The byte range of the placeholder.
    pub range: Range<usize>,
    /// How it is written.
    pub kind: PlaceholderKind,
}

/// Finds the parameter placeholders of `text`, ascending by position.
///
/// Per dialect: `PostgreSQL` has `$n` and `:name`; `MySQL` has `?` and `:name`; `SQLite` has
/// `?`, `?n`, `:name`, `@name` and `$name`. Text in strings, quoted identifiers, comments and
/// dollar quotes yields none. On `PostgreSQL` a `:name` inside `[ ]` is an array slice and is
/// skipped. `$0`, `?0` and numbers above `u32::MAX` are dropped. Compound statements (triggers,
/// procedures, functions, events) yield none, because their bodies name routine arguments.
///
/// Pure and linear in the size of `text`.
#[must_use]
pub fn placeholders(dialect: Dialect, text: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    for seg in split(dialect, text) {
        if seg.kind != SegmentKind::Statement {
            continue;
        }
        let base = seg.body.start;
        let Some(body) = text.get(seg.body) else {
            continue;
        };
        let tokens = tokenize(dialect, body);
        if is_compound(dialect, body, &tokens) {
            continue;
        }
        let mut brackets = 0usize;
        for tok in &tokens {
            match tok.kind {
                TokenKind::Punct if dialect == Dialect::Postgres => {
                    match body.as_bytes()[tok.start] {
                        b'[' => brackets += 1,
                        b']' => brackets = brackets.saturating_sub(1),
                        _ => {}
                    }
                }
                TokenKind::Param if brackets == 0 => {
                    let t = tok.text(body);
                    if let Some(kind) = kind_of(dialect, t) {
                        out.push(Placeholder {
                            range: base + tok.start..base + tok.end,
                            kind,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn kind_of(dialect: Dialect, t: &str) -> Option<PlaceholderKind> {
    if dialect == Dialect::TSql {
        // Only `@P<k>`: every other `@x` is a T-SQL variable.
        let digits = t.strip_prefix("@P").or_else(|| t.strip_prefix("@p"))?;
        return (digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0'))
            .then(|| digits.parse::<u32>().ok())
            .flatten()
            .map(PlaceholderKind::Positional);
    }
    if t == "?" {
        return Some(PlaceholderKind::Anonymous);
    }
    let numbered = match (dialect, t.as_bytes().first()) {
        (Dialect::Postgres, Some(b'$')) | (Dialect::Sqlite, Some(b'?')) => Some(&t[1..]),
        _ => None,
    };
    match numbered {
        // Digits only: `$1` on `PostgreSQL`, `?1` on `SQLite`. Zero and overflow are dropped.
        Some(n) => n
            .parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
            .map(PlaceholderKind::Positional),
        None => Some(PlaceholderKind::Named(t.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(d: Dialect, s: &str) -> Vec<&str> {
        placeholders(d, s)
            .iter()
            .map(|p| &s[p.range.clone()])
            .collect()
    }

    #[test]
    fn kinds() {
        let p = placeholders(Dialect::Sqlite, "select ?, ?2, :a, @b, $c, $3x");
        let k: Vec<_> = p.into_iter().map(|p| p.kind).collect();
        assert_eq!(
            k,
            vec![
                PlaceholderKind::Anonymous,
                PlaceholderKind::Positional(2),
                PlaceholderKind::Named(":a".into()),
                PlaceholderKind::Named("@b".into()),
                PlaceholderKind::Named("$c".into()),
                PlaceholderKind::Named("$3x".into()),
            ]
        );
    }

    #[test]
    fn zero_and_overflow_are_dropped() {
        assert_eq!(found(Dialect::Postgres, "select $0, $99999999999").len(), 0);
        assert_eq!(found(Dialect::Sqlite, "select ?0").len(), 0);
        assert_eq!(
            found(Dialect::Postgres, "select $4294967295"),
            ["$4294967295"]
        );
    }

    #[test]
    fn tsql_has_only_p_numbered_placeholders() {
        let t = Dialect::TSql;
        let p = placeholders(
            t,
            "select @P1, @p2, @P10, @x, @@rowcount, @P0, @P01, @P, @Px, ?, :a",
        );
        let k: Vec<_> = p.into_iter().map(|p| p.kind).collect();
        assert_eq!(
            k,
            vec![
                PlaceholderKind::Positional(1),
                PlaceholderKind::Positional(2),
                PlaceholderKind::Positional(10),
            ]
        );
        assert_eq!(found(t, "select @P4294967295"), ["@P4294967295"]);
        assert_eq!(found(t, "select @P4294967296, @P99999999999").len(), 0);
        // Not in strings, quoted names, comments or routines.
        assert_eq!(
            found(t, "select '@P1', [@P2], \"@P3\" -- @P4\n/* @P5 */, @P6"),
            ["@P6"]
        );
        assert_eq!(found(t, "create proc p @P1 int as select @P1").len(), 0);
        assert_eq!(found(t, "select 1\nGO\nselect @P1"), ["@P1"]);
    }

    #[test]
    fn compound_has_none() {
        assert_eq!(
            found(
                Dialect::Sqlite,
                "create trigger t after insert on x begin select :a; end"
            )
            .len(),
            0
        );
    }
}
