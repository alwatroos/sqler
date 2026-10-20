// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Statement selection, prefix and qualifier detection, and the completion context.

use std::ops::Range;

use sqler_sql::Dialect;

use crate::lexer::{TokKind, Token, lex};
use crate::scope::{Clause, Fs, Ident, LevelKind, Scope, Snapshot, ident_from_token, scan};

/// What the cursor is completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContextKind {
    /// Nothing to complete: inside a string or comment, after `AS`, after a number, and so on.
    None,
    /// A statement can start here.
    StatementStart,
    /// A table, view or schema is expected (after `FROM`, `JOIN`, `INTO`, `UPDATE`).
    Relation,
    /// After a complete relation reference: `JOIN`, `ON`, `WHERE`, ...
    RelationTail,
    /// An expression: select list, `WHERE`, `ON`, `GROUP BY`, `ORDER BY`, `SET`, ...
    Expression,
    /// After `name.` or `schema.table.`.
    Qualified,
    /// Inside the column list of `INSERT INTO t (...)`.
    InsertColumns,
}

/// The result of analysing the text around the cursor, before any schema is consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Analysis {
    /// Byte range of the statement under the cursor (without the delimiting `;`).
    pub statement: Range<usize>,
    /// What is being completed.
    pub context: ContextKind,
    /// The range a completion replaces: the identifier fragment at the cursor, or an empty range.
    pub prefix: Range<usize>,
    /// The identifiers before the dot for [`ContextKind::Qualified`], outermost first.
    pub qualifier: Vec<Ident>,
    /// The names visible at the cursor.
    pub scope: Scope,
}

/// Floors `offset` to a char boundary inside `text`.
pub(crate) fn clamp_offset(text: &str, offset: usize) -> usize {
    let mut o = offset.min(text.len());
    while !text.is_char_boundary(o) {
        o -= 1;
    }
    o
}

/// Analyses `text` at byte `offset` (clamped to the text and floored to a char boundary).
#[must_use]
pub fn analyze(text: &str, offset: usize, dialect: Dialect) -> Analysis {
    let cursor = clamp_offset(text, offset);
    let toks = lex(text, dialect);
    let idx = toks.partition_point(|t| t.start < cursor);
    let is_semi = |t: &Token| t.is_punct(text, ';');

    let first = toks[..idx].iter().rposition(is_semi).map_or(0, |p| p + 1);
    let stmt_start = if first == 0 { 0 } else { toks[first - 1].end };
    let last = toks[idx..]
        .iter()
        .position(is_semi)
        .map_or(toks.len(), |p| idx + p);
    let stmt_end = toks.get(last).map_or(text.len(), |t| t.start);
    let statement = stmt_start..stmt_end.max(stmt_start);

    let empty = |context| Analysis {
        statement: statement.clone(),
        context,
        prefix: cursor..cursor,
        qualifier: Vec::new(),
        scope: Scope::default(),
    };

    // The token that ends at or contains the cursor.
    let touching = idx.checked_sub(1).map(|k| toks[k]);
    if let Some(t) = touching {
        let inside = match t.kind {
            TokKind::Str | TokKind::Comment => {
                cursor < t.end || (cursor == t.end && t.unterminated)
            }
            TokKind::Number => cursor <= t.end,
            _ => false,
        };
        if inside {
            return empty(ContextKind::None);
        }
    }

    let stmt: Vec<Token> = toks[first..last.max(first)]
        .iter()
        .copied()
        .filter(|t| t.kind != TokKind::Comment)
        .collect();

    // The identifier fragment at the cursor.
    let mut prefix = cursor..cursor;
    if let Some(t) = touching
        && t.start >= stmt_start
    {
        match t.kind {
            TokKind::Word if cursor <= t.end => prefix = t.start..cursor,
            TokKind::Quoted if cursor < t.end || (cursor == t.end && t.unterminated) => {
                prefix = t.start..t.end;
            }
            _ => {}
        }
    }
    let skip = prefix.start < prefix.end;

    let before: Vec<&Token> = stmt.iter().filter(|t| t.end <= prefix.start).collect();
    let last_tok = before.last().copied();
    let qualifier = qualifier_of(text, &before);
    let dotted = last_tok.is_some_and(|t| t.is_punct(text, '.'));

    let scan = scan(text, &stmt, prefix.start, skip, dialect);
    let scope = scan.scope();
    let context = if dotted {
        if qualifier.is_empty() {
            ContextKind::None
        } else {
            ContextKind::Qualified
        }
    } else {
        context_of(&scan.top, last_tok, text)
    };
    Analysis {
        statement,
        context,
        prefix,
        qualifier,
        scope,
    }
}

/// `a . b . |` -> `[a, b]`.
fn qualifier_of(text: &str, before: &[&Token]) -> Vec<Ident> {
    let mut out = Vec::new();
    let mut j = before.len();
    if j == 0 || !before[j - 1].is_punct(text, '.') {
        return out;
    }
    while j >= 2 && before[j - 1].is_punct(text, '.') {
        let t = before[j - 2];
        if !matches!(t.kind, TokKind::Word | TokKind::Quoted) {
            break;
        }
        out.push(ident_from_token(text, t));
        j -= 2;
    }
    out.reverse();
    out
}

fn context_of(s: &Snapshot, last: Option<&Token>, text: &str) -> ContextKind {
    if last.is_some_and(|t| t.kind == TokKind::Word && t.text(text).eq_ignore_ascii_case("as")) {
        return ContextKind::None;
    }
    match s.kind {
        LevelKind::InsertCols => return ContextKind::InsertColumns,
        LevelKind::ValuesRow | LevelKind::CteCols => return ContextKind::None,
        _ => {}
    }
    match s.clause {
        Clause::Start => ContextKind::StatementStart,
        Clause::With => {
            if last.is_some_and(|t| t.is_punct(text, ')')) {
                ContextKind::StatementStart
            } else {
                ContextKind::None
            }
        }
        Clause::Select
        | Clause::Where
        | Clause::On
        | Clause::Group
        | Clause::Order
        | Clause::Having
        | Clause::Set
        | Clause::Returning
        | Clause::Expr => ContextKind::Expression,
        Clause::From | Clause::Update | Clause::Into => match s.fs {
            Fs::Expect => ContextKind::Relation,
            Fs::Name | Fs::Done => ContextKind::RelationTail,
            Fs::NameDot | Fs::AsKw => ContextKind::None,
        },
        Clause::Insert | Clause::Delete => ContextKind::RelationTail,
        Clause::Values | Clause::Other => ContextKind::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::RelationSource;

    /// Analyses `sql` with `|` marking the cursor.
    fn at(sql: &str, d: Dialect) -> (Analysis, String) {
        let pos = sql.find('|').expect("cursor marker");
        let text = sql.replacen('|', "", 1);
        (analyze(&text, pos, d), text)
    }

    fn ctx(sql: &str) -> ContextKind {
        at(sql, Dialect::Postgres).0.context
    }

    #[test]
    fn contexts_per_clause() {
        use ContextKind::*;
        let cases = [
            ("|", StatementStart),
            ("sel|", StatementStart),
            ("SELECT 1; |", StatementStart),
            ("SELECT | FROM t", Expression),
            ("SELECT a, |", Expression),
            ("SELECT * FROM |", Relation),
            ("SELECT * FROM us|", Relation),
            ("SELECT * FROM a, |", Relation),
            ("SELECT * FROM a JOIN |", Relation),
            ("SELECT * FROM a LEFT JOIN |", Relation),
            ("SELECT * FROM a |", RelationTail),
            ("SELECT * FROM a x |", RelationTail),
            ("SELECT * FROM a AS |", None),
            ("SELECT * FROM a JOIN b ON |", Expression),
            ("SELECT * FROM a JOIN b ON a.id = b.id, |", Relation),
            ("SELECT * FROM a WHERE |", Expression),
            ("SELECT * FROM a GROUP BY |", Expression),
            ("SELECT * FROM a GROUP BY x HAVING |", Expression),
            ("SELECT * FROM a ORDER BY |", Expression),
            ("SELECT * FROM a LIMIT |", None),
            ("SELECT * FROM (|", StatementStart),
            ("SELECT * FROM a WHERE x IN (|", Expression),
            ("SELECT * FROM a WHERE x IN (SELECT y FROM |", Relation),
            ("SELECT count(|", Expression),
            ("SELECT * FROM a WHERE a.|", Qualified),
            ("SELECT public.users.|", Qualified),
            ("INSERT INTO |", Relation),
            ("INSERT INTO t (|", InsertColumns),
            ("INSERT INTO t (a, |", InsertColumns),
            ("INSERT INTO t |", RelationTail),
            ("INSERT INTO t VALUES (|", None),
            ("UPDATE |", Relation),
            ("UPDATE t SET |", Expression),
            ("UPDATE t SET a = 1 WHERE |", Expression),
            ("DELETE FROM |", Relation),
            ("DELETE FROM t WHERE |", Expression),
            ("DELETE |", RelationTail),
            ("INSERT INTO t SELECT * FROM x RETURNING |", Expression),
            ("WITH x AS (SELECT 1) |", StatementStart),
            ("WITH |", None),
            ("WITH x AS (SELECT * FROM |", Relation),
            ("SELECT 1 UNION |", StatementStart),
            ("SELECT 'abc|", None),
            ("SELECT 'a|bc'", None),
            ("SELECT 'abc'| FROM t", Expression),
            ("SELECT 1 -- hello|", None),
            ("SELECT /* hel|lo */ 1", None),
            ("SELECT 12|", None),
            ("SELECT a AS |", None),
            ("SELECT extract(year FROM |", Expression),
            ("DROP TABLE |", Relation),
            ("SELECT 1; SELECT | FROM t", Expression),
        ];
        for (sql, want) in cases {
            assert_eq!(ctx(sql), want, "{sql}");
        }
    }

    #[test]
    fn prefix_and_statement() {
        let (a, text) = at("SELECT 1; SELECT na| FROM t; SELECT 3", Dialect::Postgres);
        assert_eq!(&text[a.statement.clone()], " SELECT na FROM t");
        assert_eq!(&text[a.prefix.clone()], "na");

        let (a, text) = at("SELECT \"Ord|", Dialect::Postgres);
        assert_eq!(&text[a.prefix.clone()], "\"Ord");
        assert_eq!(a.context, ContextKind::Expression);

        let (a, text) = at("SELECT 1;| SELECT 2", Dialect::Postgres);
        assert_eq!(&text[a.statement], " SELECT 2");
        let (a, text) = at("SELECT 1|; SELECT 2", Dialect::Postgres);
        assert_eq!(&text[a.statement], "SELECT 1");
    }

    #[test]
    fn offsets_are_clamped_to_boundaries() {
        let text = "SELECT 'é' FROM t";
        for offset in 0..text.len() + 5 {
            let a = analyze(text, offset, Dialect::Postgres);
            assert!(text.is_char_boundary(a.prefix.start));
            assert!(text.is_char_boundary(a.prefix.end));
            assert!(a.prefix.end <= text.len());
        }
    }

    #[test]
    fn qualifier_chain() {
        let (a, _) = at("SELECT public.\"My T\".|", Dialect::Postgres);
        let q: Vec<_> = a.qualifier.iter().map(|i| i.value.as_str()).collect();
        assert_eq!(q, ["public", "My T"]);
        assert!(a.qualifier[1].quoted);
    }

    #[test]
    fn scope_aliases_and_ctes() {
        let (a, _) = at(
            "WITH recent AS (SELECT id, total AS amount FROM orders) \
             SELECT u.| FROM public.users AS u JOIN recent r ON true, orders",
            Dialect::Postgres,
        );
        assert_eq!(a.context, ContextKind::Qualified);
        let names: Vec<_> = a
            .scope
            .relations
            .iter()
            .map(|r| r.visible_name().map(|i| i.value.clone()))
            .collect();
        assert_eq!(
            names,
            [
                Some("u".to_owned()),
                Some("r".to_owned()),
                Some("orders".to_owned())
            ]
        );
        let u = &a.scope.relations[0];
        assert!(matches!(
            &u.source,
            RelationSource::Table { schema: Some(s), name } if s.value == "public" && name.value == "users"
        ));
        assert!(matches!(
            &a.scope.relations[1].source,
            RelationSource::Cte { .. }
        ));
        assert_eq!(a.scope.ctes.len(), 1);
        assert_eq!(a.scope.ctes[0].columns, ["id", "amount"]);
        let u_id = Ident::new("U", false);
        assert!(a.scope.resolve(&u_id, Dialect::Postgres).is_some());
        let quoted = Ident::new("U", true);
        assert!(a.scope.resolve(&quoted, Dialect::Postgres).is_none());
        assert!(a.scope.resolve(&quoted, Dialect::MySql).is_some());
        let bare = Ident::new("orders", false);
        assert!(a.scope.resolve(&bare, Dialect::Postgres).is_some());
    }

    #[test]
    fn scope_subquery_columns_and_levels() {
        let (a, _) = at(
            "SELECT * FROM (SELECT u.id, u.name AS who, count(*) c, count(*), x.* FROM users u) s \
             JOIN o ON s.|",
            Dialect::Postgres,
        );
        let s = a.scope.resolve(&Ident::new("s", false), Dialect::Postgres);
        let Some(s) = s else { panic!("subquery alias") };
        assert!(matches!(
            &s.source,
            RelationSource::Subquery { columns } if columns == &["id", "who", "c"]
        ));

        // Inside the subquery: its own relations first, then the outer ones.
        let (a, _) = at(
            "SELECT * FROM outer_t o WHERE EXISTS (SELECT 1 FROM inner_t i WHERE i.x = |)",
            Dialect::Postgres,
        );
        let names: Vec<_> = a
            .scope
            .relations
            .iter()
            .filter_map(|r| r.visible_name().map(|i| i.value.as_str()))
            .collect();
        assert_eq!(names, ["i", "o"]);
        assert_eq!(a.scope.local, 1);
    }

    #[test]
    fn cte_with_declared_columns_and_unterminated() {
        let (a, _) = at(
            "WITH t (a, b) AS (SELECT 1, 2), u AS (SELECT * FROM t) SELECT | FROM u",
            Dialect::Postgres,
        );
        assert_eq!(a.scope.ctes.len(), 2);
        assert_eq!(a.scope.ctes[0].columns, ["a", "b"]);
        let (a, _) = at("SELECT * FROM (SELECT | FROM t", Dialect::Postgres);
        assert_eq!(a.scope.relations.len(), 1);
        let (a, _) = at("SELECT * FROM \"My Items\" mi WHERE mi.|", Dialect::Sqlite);
        assert!(a.scope.relations[0].alias.is_some());
    }

    #[test]
    fn cursor_before_from_sees_the_from_clause() {
        let (a, _) = at("SELECT u.| FROM users u", Dialect::MySql);
        assert_eq!(a.context, ContextKind::Qualified);
        assert_eq!(a.scope.relations.len(), 1);
    }
}
