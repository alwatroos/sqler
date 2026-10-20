// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Diagnostics: unknown schemas, tables, columns and qualifiers, and ambiguous columns.
//!
//! Deliberately conservative. A name is reported only when the schema source can say that it does
//! not exist; anything that could belong to unloaded data, to a table created in the text, or to
//! syntax the heuristic scanner does not model is left alone. See the suppression rules in
//! `docs/src/architecture/completion.md`.

use std::ops::Range;

use sqler_sql::Dialect;

use crate::dialect_data::{
    Flavor, eq_fold, flavor, idents_match, is_non_column_word, is_reserved, is_system_column,
    stored_eq_strict,
};
use crate::lexer::{TokKind, Token, lex};
use crate::resolve::{Chain, Cols, RelLookup, Resolver, is_expr_clause, read_chain};
use crate::scope::{
    Clause, Ident, LevelKind, RelationSource, Scan, Snapshot, is_scan_keyword, scan_all,
};
use crate::source::SchemaSource;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Severity {
    /// The statement fails.
    Error,
    /// The statement may not do what was meant.
    Warning,
    /// A note.
    Info,
}

/// What a [`Diagnostic`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum DiagnosticCode {
    /// A schema that the source does not list.
    UnknownSchema,
    /// A table or view that does not exist in its (loaded) schema.
    UnknownRelation,
    /// A column that its (loaded) relation does not have.
    UnknownColumn,
    /// A qualifier that is neither an alias nor a table in scope.
    UnknownQualifier,
    /// An unqualified column that more than one relation in scope has.
    AmbiguousColumn,
}

/// One finding in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {
    /// The byte range of the identifier, quotes included.
    pub range: Range<usize>,
    /// How serious it is.
    pub severity: Severity,
    /// What it is about.
    pub code: DiagnosticCode,
    /// A message for the user; names are shown as written.
    pub message: String,
}

/// Words that sit between two operands without being one (so the name after them is not an
/// implicit alias).
const OPERATOR_WORDS: &[&str] = &[
    "ilike", "similar", "nulls", "escape", "isnull", "notnull", "xor", "overlaps", "against",
];

/// Words that end an operand (the name after them is an implicit alias).
const OPERAND_WORDS: &[&str] = &["end", "null", "true", "false"];

/// Statements that are checked: they read or write table data. Everything else (DDL, `SET`,
/// `SHOW`, ...) is skipped.
const CHECKED_STATEMENTS: &[&str] = &[
    "select", "with", "insert", "replace", "update", "delete", "table", "values", "explain",
];

/// Finds the problems in `text`, statement by statement. Pure and total.
///
/// Sorted by position. Names that cannot be judged (relations whose columns are not loaded,
/// system schemas, tables created in the text, ...) are never reported. PostgreSQL names are
/// matched the way the server does: an unquoted name folds to lower case, a quoted one is exact;
/// MySQL and SQLite compare case-insensitively.
#[must_use]
pub fn diagnose(text: &str, dialect: Dialect, schema: &dyn SchemaSource) -> Vec<Diagnostic> {
    let toks: Vec<Token> = lex(text, dialect)
        .into_iter()
        .filter(|t| t.kind != TokKind::Comment)
        .collect();
    let mut res = Resolver::new(schema, dialect, true, true);
    let created = created_names(text, &toks);
    res.locals = created.relations;
    res.local_schemas = created.schemas;
    let mut out = Vec::new();
    for stmt in toks.split(|t| t.is_punct(text, ';')) {
        let Some(first) = stmt.first() else { continue };
        let checked = first.is_punct(text, '(')
            || (first.kind == TokKind::Word
                && CHECKED_STATEMENTS
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(first.text(text))));
        if checked {
            let scan = scan_all(text, stmt, dialect);
            Checker::new(text, stmt, dialect, &scan, &mut res).run(&mut out);
        }
    }
    // Parameters, DECLAREd locals and NEW/OLD rows look like columns but are not: nothing inside a
    // routine definition is reported.
    let bodies = routine_bodies(text, &toks);
    if !bodies.is_empty() {
        out.retain(|d| !bodies.iter().any(|b| b.contains(&d.range.start)));
    }
    out.sort_by(|a, b| {
        a.range
            .start
            .cmp(&b.range.start)
            .then(a.code.cmp(&b.code))
            .then(a.range.end.cmp(&b.range.end))
    });
    out.dedup();
    out
}

/// Whether token `t` is the first thing on its line (`DELIMITER` is a client command only there).
fn starts_line(text: &str, t: &Token) -> bool {
    text.get(..t.start).is_some_and(|before| {
        before
            .rsplit('\n')
            .next()
            .is_some_and(|l| l.trim().is_empty())
    })
}

/// The byte ranges of stored-routine definitions (`CREATE`/`ALTER` `PROCEDURE`, `FUNCTION`,
/// `TRIGGER`, `EVENT`). Names in them may be parameters, variables or `NEW`/`OLD` rows, so nothing
/// inside is reported.
///
/// Under a custom `DELIMITER` (MySQL clients) a range runs to the next `DELIMITER` line, or the end
/// of the text. Otherwise it ends at the routine's own closing `;`, outside any `BEGIN ... END`
/// or `CASE ... END` nesting (a dollar-quoted body is a single token), so later statements are
/// still checked.
fn routine_bodies(text: &str, toks: &[Token]) -> Vec<Range<usize>> {
    let is = |i: usize, w: &str| {
        toks.get(i)
            .is_some_and(|t| t.kind == TokKind::Word && t.text(text).eq_ignore_ascii_case(w))
    };
    let delimiter_line = |i: usize| is(i, "delimiter") && starts_line(text, &toks[i]);
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut custom = false;
    let mut i = 0;
    while i < toks.len() {
        if delimiter_line(i) {
            let arg = text.get(toks[i].end..).and_then(|r| r.lines().next());
            custom = arg.is_some_and(|a| {
                let a = a.trim();
                !a.is_empty() && a != ";"
            });
        } else if is(i, "create") || is(i, "alter") {
            // Skip modifiers and a `DEFINER = user@host` clause up to the object kind.
            let kind = (i + 1..toks.len().min(i + 16)).find(|&j| {
                ["procedure", "function", "trigger", "event"]
                    .iter()
                    .any(|k| is(j, k))
            });
            let stopped = (i + 1..kind.unwrap_or(i)).any(|j| {
                [
                    "table", "view", "index", "database", "schema", "sequence", "type",
                ]
                .iter()
                .any(|k| is(j, k))
            });
            if kind.is_some() && !stopped {
                let end = routine_end(text, toks, i, custom);
                let end_byte = toks.get(end).map_or(text.len(), |t| {
                    if custom || !t.is_punct(text, ';') {
                        t.start
                    } else {
                        t.end
                    }
                });
                out.push(toks[i].start..end_byte);
                i = end.max(i + 1);
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The index of the token that ends the routine starting at `start`: the next `DELIMITER` line, or
/// (without a custom delimiter) the closing `;` at nesting depth zero. `toks.len()` when neither.
fn routine_end(text: &str, toks: &[Token], start: usize, custom: bool) -> usize {
    let word = |i: usize| {
        toks.get(i)
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text(text))
    };
    let is = |i: usize, w: &str| word(i).is_some_and(|x| x.eq_ignore_ascii_case(w));
    let mut depth = 0usize;
    let mut i = start + 1;
    while i < toks.len() {
        if is(i, "delimiter") && starts_line(text, &toks[i]) {
            return i;
        }
        if !custom {
            if is(i, "begin") || is(i, "case") {
                depth += 1;
            } else if is(i, "end") {
                if ["if", "loop", "while", "repeat"]
                    .iter()
                    .any(|k| is(i + 1, k))
                {
                    i += 1;
                } else {
                    depth = depth.saturating_sub(1);
                    if is(i + 1, "case") {
                        i += 1;
                    }
                }
            } else if depth == 0 && toks[i].is_punct(text, ';') {
                return i;
            }
        }
        i += 1;
    }
    toks.len()
}

/// Names the text itself creates, renames or alters: tables and views (their columns cannot be
/// judged), and schemas (`CREATE SCHEMA`, `ATTACH ... AS`).
#[derive(Default)]
struct Created {
    relations: Vec<Ident>,
    schemas: Vec<Ident>,
}

/// Finds what the text creates, alters, renames or attaches anywhere in it. Whatever is found is
/// only ever used to stay silent.
#[allow(clippy::too_many_lines)]
fn created_names(text: &str, toks: &[Token]) -> Created {
    let word = |i: usize| {
        toks.get(i)
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text(text))
    };
    let is = |i: usize, w: &str| word(i).is_some_and(|x| x.eq_ignore_ascii_case(w));
    let skip_if_exists = |mut j: usize| {
        if is(j, "if") && (is(j + 1, "not") || is(j + 1, "exists")) {
            j += if is(j + 1, "not") { 3 } else { 2 };
        }
        j
    };
    let mut out = Created::default();
    // Whether the current statement starts with SELECT/WITH and has no INSERT/MERGE in it.
    let mut select_stmt = false;
    let mut stmt_start = true;
    for i in 0..toks.len() {
        if toks[i].is_punct(text, ';') {
            stmt_start = true;
            continue;
        }
        if stmt_start {
            select_stmt = is(i, "select") || is(i, "with");
            stmt_start = false;
        }
        if ["insert", "replace", "merge", "update", "delete"]
            .iter()
            .any(|w| is(i, w))
        {
            select_stmt = false;
        }
        if is(i, "create") {
            let mut j = i + 1;
            while [
                "or",
                "replace",
                "temp",
                "temporary",
                "unlogged",
                "materialized",
                "global",
                "local",
                "recursive",
                "virtual",
            ]
            .iter()
            .any(|w| is(j, w))
            {
                j += 1;
            }
            if is(j, "table") || is(j, "view") {
                j = skip_if_exists(j + 1);
                if let Some(name) = read_chain(text, toks, j).parts.pop() {
                    out.relations.push(name);
                }
            } else if is(j, "schema") || is(j, "database") {
                j = skip_if_exists(j + 1);
                if let Some(name) = read_chain(text, toks, j).parts.pop() {
                    out.schemas.push(name);
                }
            }
        } else if is(i, "alter") {
            let mut j = i + 1;
            while ["materialized", "temporary"].iter().any(|w| is(j, w)) {
                j += 1;
            }
            if is(j, "table") || is(j, "view") {
                j = skip_if_exists(j + 1);
                if is(j, "only") {
                    j += 1;
                }
                if let Some(name) = read_chain(text, toks, j).parts.pop() {
                    out.relations.push(name);
                }
            }
        } else if is(i, "rename") {
            // `RENAME TO new`, `RENAME AS new`, MySQL `RENAME TABLE a TO b, c TO d`.
            if is(i + 1, "to") || is(i + 1, "as") {
                if let Some(name) = read_chain(text, toks, i + 2).parts.pop() {
                    out.relations.push(name);
                }
            } else if is(i + 1, "table") {
                let mut j = i + 2;
                loop {
                    let from = read_chain(text, toks, j);
                    if let Some(name) = from.parts.last() {
                        out.relations.push(name.clone());
                    }
                    if !is(from.next, "to") {
                        break;
                    }
                    let to = read_chain(text, toks, from.next + 1);
                    if let Some(name) = to.parts.last() {
                        out.relations.push(name.clone());
                    }
                    if !toks.get(to.next).is_some_and(|t| t.is_punct(text, ',')) {
                        break;
                    }
                    j = to.next + 1;
                }
            }
        } else if is(i, "attach") {
            let mut j = i + 1;
            while let Some(t) = toks.get(j) {
                if t.is_punct(text, ';') || is(j, "as") {
                    break;
                }
                j += 1;
            }
            if is(j, "as")
                && let Some(name) = read_chain(text, toks, j + 1).parts.pop()
            {
                out.schemas.push(name);
            }
        } else if is(i, "into") && select_stmt {
            // PostgreSQL / SQL Server `SELECT ... INTO [TEMP] [TABLE] new_table`.
            let mut j = i + 1;
            while ["temp", "temporary", "unlogged", "table"]
                .iter()
                .any(|w| is(j, w))
            {
                j += 1;
            }
            if let Some(name) = read_chain(text, toks, j).parts.pop() {
                out.relations.push(name);
            }
        }
    }
    out
}

struct Checker<'r, 'a, 's> {
    text: &'a str,
    toks: &'a [Token],
    dialect: Dialect,
    res: &'r mut Resolver<'a>,
    scan: &'s Scan,
    /// `USING` or `NATURAL` anywhere: same-named columns are merged, not ambiguous.
    merged: bool,
    /// Per token: whether the parenthesis group it sits in (or closes) is followed by `=`, as in
    /// `SET (a, b) = ...`. Computed once per statement.
    group_assigned: Vec<bool>,
}

/// For every token, whether the innermost parenthesis group around it is directly followed by
/// `=`. One pass, no rescanning.
fn group_assigned(text: &str, toks: &[Token]) -> Vec<bool> {
    let mut enclosing: Vec<Option<usize>> = Vec::with_capacity(toks.len());
    let mut closed_at: Vec<Option<usize>> = vec![None; toks.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (k, t) in toks.iter().enumerate() {
        if t.is_punct(text, ')') {
            let open = stack.pop();
            if let Some(o) = open {
                closed_at[o] = Some(k);
            }
            enclosing.push(open);
        } else {
            enclosing.push(stack.last().copied());
            if t.is_punct(text, '(') {
                stack.push(k);
            }
        }
    }
    enclosing
        .iter()
        .map(|e| {
            e.and_then(|o| closed_at[o])
                .and_then(|c| toks.get(c + 1))
                .is_some_and(|t| t.is_punct(text, '='))
        })
        .collect()
}

impl<'r, 'a: 's, 's> Checker<'r, 'a, 's> {
    fn new(
        text: &'a str,
        toks: &'a [Token],
        dialect: Dialect,
        scan: &'s Scan,
        res: &'r mut Resolver<'a>,
    ) -> Self {
        let merged = toks.iter().any(|t| {
            t.kind == TokKind::Word && {
                let w = t.text(text);
                w.eq_ignore_ascii_case("using") || w.eq_ignore_ascii_case("natural")
            }
        });
        Self {
            text,
            toks,
            dialect,
            res,
            scan,
            merged,
            group_assigned: group_assigned(text, toks),
        }
    }

    fn run(mut self, out: &mut Vec<Diagnostic>) {
        self.relations(out);
        // Column cache per query level: the columns of each of its relations.
        let mut cache: Vec<Option<Vec<Cols<'s>>>> = vec![None; self.scan.level_count()];
        let mut i = 0;
        let toks = self.toks;
        while let Some(t) = toks.get(i) {
            if !matches!(t.kind, TokKind::Word | TokKind::Quoted) {
                i += 1;
                continue;
            }
            if t.kind == TokKind::Word && t.text(self.text).eq_ignore_ascii_case("on") {
                let next = self.toks.get(i + 1).filter(|n| n.kind == TokKind::Word);
                if next.is_some_and(|n| {
                    let w = n.text(self.text);
                    w.eq_ignore_ascii_case("conflict") || w.eq_ignore_ascii_case("duplicate")
                }) {
                    break;
                }
            }
            // A member access such as `(x).field` is not a column reference.
            let after_dot = i > 0 && self.toks[i - 1].is_punct(self.text, '.');
            let chain = read_chain(self.text, self.toks, i);
            let start = i;
            i = chain.next.max(i + 1);
            if after_dot {
                continue;
            }
            self.chain(start, &chain, &mut cache, out);
        }
    }

    fn relations(&mut self, out: &mut Vec<Diagnostic>) {
        for level in 0..self.scan.level_count() {
            for rel in self.scan.relations(level) {
                let RelationSource::Table { schema, name } = &rel.source else {
                    continue;
                };
                if schema.is_none() && self.scan.find_cte(level, name, self.dialect).is_some() {
                    continue;
                }
                match self.res.relation(schema.as_ref(), name) {
                    RelLookup::UnknownSchema => {
                        if let Some(s) = schema {
                            out.push(diag(
                                &s.span,
                                DiagnosticCode::UnknownSchema,
                                format!("unknown schema \"{}\"", s.value),
                            ));
                        }
                    }
                    RelLookup::Unknown => {
                        let shown = match schema {
                            Some(s) => format!("{}.{}", s.value, name.value),
                            None => name.value.clone(),
                        };
                        out.push(diag(
                            &name.span,
                            DiagnosticCode::UnknownRelation,
                            format!("unknown table or view \"{shown}\""),
                        ));
                    }
                    RelLookup::Found(_) | RelLookup::NotKnowable => {}
                }
            }
        }
    }

    /// Fills the column cache for `level` and the levels around it.
    fn fill(&mut self, level: usize, cache: &mut [Option<Vec<Cols<'s>>>]) {
        let mut next = Some(level);
        while let Some(l) = next {
            if cache.get(l).is_some_and(Option::is_none) {
                let scan = self.scan;
                let cols: Vec<Cols<'s>> = scan
                    .relations(l)
                    .iter()
                    .map(|r| self.res.columns(r, scan, l))
                    .collect();
                cache[l] = Some(cols);
            }
            next = self.scan.parent(l);
        }
    }

    fn chain(
        &mut self,
        start: usize,
        chain: &Chain,
        cache: &mut [Option<Vec<Cols<'s>>>],
        out: &mut Vec<Diagnostic>,
    ) {
        if chain.dangling || chain.parts.len() > 3 {
            return;
        }
        let Some(snap) = self.scan.snaps.get(start).copied() else {
            return;
        };
        let insert_cols = snap.kind == LevelKind::InsertCols;
        let clause = self.scan.effective_clause(&snap);
        if !insert_cols && !is_expr_clause(clause) {
            return;
        }
        if self.skip_chain(start, chain, &snap, insert_cols) {
            return;
        }
        if insert_cols {
            self.insert_column(snap, chain, cache, out);
            return;
        }
        let level = self.scan.query_level(snap.level);
        let branch = snap.branch;
        // After a set operation, `ORDER BY` / `LIMIT` apply to the whole result: they resolve
        // against output names and positions, which are not modelled. Stay silent.
        if branch > 0 && clause == Clause::Order {
            return;
        }
        self.fill(level, cache);
        match chain.parts.as_slice() {
            [id] => {
                let target = clause == Clause::Set && self.is_set_target(chain);
                self.unqualified(id, level, branch, clause, target, cache, out);
            }
            [q, c] => self.qualified(q, c, level, branch, cache, out),
            [s, t, c] => self.schema_qualified(s, t, c, level, branch, cache, out),
            _ => {}
        }
    }

    /// Whether the chain is the left side of an assignment in `SET`: `SET a = ...` or
    /// `SET (a, b) = ...`.
    fn is_set_target(&self, chain: &Chain) -> bool {
        let text = self.text;
        let is_eq = |k: usize| self.toks.get(k).is_some_and(|t| t.is_punct(text, '='));
        if is_eq(chain.next) {
            return true;
        }
        // Inside a parenthesised target list: the token after the closing parenthesis is `=`.
        self.group_assigned
            .get(chain.next)
            .copied()
            .unwrap_or(false)
    }

    /// The rules that leave a chain alone.
    fn skip_chain(&self, start: usize, chain: &Chain, snap: &Snapshot, insert_cols: bool) -> bool {
        let text = self.text;
        let toks = self.toks;
        let Some(first) = toks.get(start) else {
            return true;
        };
        let single = chain.parts.len() == 1;
        if chain
            .toks
            .iter()
            .any(|&k| toks.get(k).is_some_and(|t| t.unterminated))
        {
            return true;
        }
        if single && first.kind == TokKind::Word && is_scan_keyword(first.text(text)) {
            return true;
        }
        if single
            && first.kind == TokKind::Quoted
            && flavor(self.dialect) == Flavor::Sqlite
            && first.text(text).starts_with('"')
        {
            // SQLite reads an unknown "double quoted" name as a string.
            return true;
        }
        // `U&'...'` / `U&"..."`: a Unicode-escape literal or identifier.
        if single
            && first.kind == TokKind::Word
            && first.text(text).eq_ignore_ascii_case("u")
            && text
                .get(first.end..)
                .is_some_and(|r| r.starts_with("&'") || r.starts_with("&\""))
        {
            return true;
        }
        let next = toks.get(chain.next);
        if let Some(n) = next {
            if n.is_punct(text, '(') && !insert_cols {
                return true;
            }
            if n.kind == TokKind::Str {
                return true;
            }
            if snap.kind == LevelKind::Expr
                && n.kind == TokKind::Word
                && n.text(text).eq_ignore_ascii_case("from")
            {
                return true;
            }
        }
        if insert_cols {
            // Only a lone name in a list: `(a, b)`, not `(SELECT ...)`.
            let prev_ok = start
                .checked_sub(1)
                .and_then(|p| toks.get(p))
                .is_some_and(|p| p.is_punct(text, '(') || p.is_punct(text, ','));
            let next_ok = next.is_some_and(|n| n.is_punct(text, ',') || n.is_punct(text, ')'));
            return !(single && prev_ok && next_ok);
        }
        let Some(prev) = start.checked_sub(1).and_then(|p| toks.get(p)) else {
            return false;
        };
        match prev.kind {
            TokKind::Punct => prev
                .text(text)
                .chars()
                .next()
                .is_some_and(|c| matches!(c, ':' | '@' | '?' | '$' | ')' | '&')),
            TokKind::Quoted | TokKind::Number | TokKind::Str => true,
            TokKind::Word => {
                let w = prev.text(text);
                if ["as", "over", "collate", "into"]
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(w))
                {
                    return true;
                }
                // `CONVERT(x USING utf8mb4)`: a character set name.
                if snap.kind == LevelKind::Expr && w.eq_ignore_ascii_case("using") {
                    return true;
                }
                OPERAND_WORDS.iter().any(|k| k.eq_ignore_ascii_case(w))
                    || !(is_scan_keyword(w)
                        || is_reserved(self.dialect, w)
                        || OPERATOR_WORDS.iter().any(|k| k.eq_ignore_ascii_case(w)))
            }
            TokKind::Comment => false,
        }
    }

    fn insert_column(
        &mut self,
        snap: Snapshot,
        chain: &Chain,
        cache: &mut [Option<Vec<Cols<'s>>>],
        out: &mut Vec<Diagnostic>,
    ) {
        let [id] = chain.parts.as_slice() else { return };
        let Some(parent) = self.scan.parent(snap.level) else {
            return;
        };
        let Some(target) = self.scan.target(parent) else {
            return;
        };
        self.fill(parent, cache);
        let cols = cache
            .get(parent)
            .and_then(Option::as_ref)
            .and_then(|c| c.get(target))
            .copied()
            .unwrap_or(Cols::Unknown);
        if matches!(cols, Cols::Unknown) || self.res.find_column(cols, id).is_some() {
            return;
        }
        out.push(unknown_column(id, None));
    }

    /// Whether anything in the scope chain makes the column list uncertain.
    fn chain_uncertain(
        &self,
        level: usize,
        branch: usize,
        cache: &[Option<Vec<Cols<'_>>>],
    ) -> bool {
        let mut any_relation = false;
        let mut next = Some((level, branch));
        while let Some((l, b)) = next {
            if self.scan.opaque(l) {
                return true;
            }
            for (ri, rel) in self.scan.branch_relations(l, b) {
                any_relation = true;
                let unknown = cache
                    .get(l)
                    .and_then(Option::as_ref)
                    .and_then(|c| c.get(ri))
                    .is_none_or(|c| matches!(c, Cols::Unknown));
                // An unnamed subquery cannot be addressed by name; treat it as unknown.
                if unknown || rel.visible_name().is_none() {
                    return true;
                }
            }
            next = self.scan.outer(l);
        }
        !any_relation
    }

    #[allow(clippy::too_many_arguments)]
    fn unqualified(
        &mut self,
        id: &Ident,
        level: usize,
        branch: usize,
        clause: Clause,
        set_target: bool,
        cache: &[Option<Vec<Cols<'s>>>],
        out: &mut Vec<Diagnostic>,
    ) {
        let dialect = self.dialect;
        if matches!(clause, Clause::Group | Clause::Order | Clause::Having)
            && self
                .scan
                .outputs(level)
                .iter()
                .any(|o| stored_eq_strict(dialect, id, o))
        {
            return;
        }
        let mut hits: Vec<usize> = Vec::new();
        let mut unknown = false;
        let in_branch = self.scan.branch_relations(level, branch).count();
        // `RETURNING` of an `INSERT` sees the insert target only; elsewhere the insert target is
        // not visible to the rows being selected.
        let insert_target = self.scan.target(level);
        let returning_only = if clause == Clause::Returning {
            insert_target
        } else {
            None
        };
        let target = if clause == Clause::Returning || in_branch == 1 {
            None
        } else {
            insert_target
        };
        let local = cache.get(level).and_then(Option::as_ref);
        for (ri, _) in self.scan.branch_relations(level, branch) {
            if Some(ri) == target || returning_only.is_some_and(|t| t != ri) {
                continue;
            }
            let cols = local.and_then(|c| c.get(ri)).copied();
            match cols {
                None | Some(Cols::Unknown) => unknown = true,
                Some(c) => {
                    if self.res.find_column(c, id).is_some() {
                        hits.push(ri);
                    }
                }
            }
        }
        match hits.as_slice() {
            [_] => return,
            [a, b, ..] => {
                // `USING`/`NATURAL` merge columns; the name left of `=` in `SET` is the update
                // target.
                if self.merged || set_target {
                    return;
                }
                let rels = self.scan.relations(level);
                let name = |ri: &usize| {
                    rels.get(*ri)
                        .and_then(|r| r.visible_name())
                        .map_or("subquery", |i| i.value.as_str())
                };
                out.push(Diagnostic {
                    range: id.span.clone(),
                    severity: Severity::Warning,
                    code: DiagnosticCode::AmbiguousColumn,
                    message: format!(
                        "column \"{}\" is ambiguous (\"{}\", \"{}\")",
                        id.value,
                        name(a),
                        name(b)
                    ),
                });
                return;
            }
            [] => {}
        }
        // Outer levels.
        let mut next = self.scan.outer(level);
        while let Some((l, b)) = next {
            let outer = cache.get(l).and_then(Option::as_ref);
            for (ri, _) in self.scan.branch_relations(l, b) {
                match outer.and_then(|c| c.get(ri)).copied() {
                    None | Some(Cols::Unknown) => unknown = true,
                    Some(c) => {
                        if self.res.find_column(c, id).is_some() {
                            return;
                        }
                    }
                }
            }
            next = self.scan.outer(l);
        }
        if unknown || self.chain_uncertain(level, branch, cache) {
            return;
        }
        if (!id.quoted && is_non_column_word(dialect, &id.value))
            || is_system_column(dialect, &id.value)
        {
            return;
        }
        // A whole-row reference: `to_jsonb(u)`, `(u).field`.
        if self
            .scan
            .resolve_qualifier(level, branch, id, dialect)
            .is_some()
        {
            return;
        }
        out.push(unknown_column(id, None));
    }

    fn qualified(
        &mut self,
        q: &Ident,
        c: &Ident,
        level: usize,
        branch: usize,
        cache: &[Option<Vec<Cols<'s>>>],
        out: &mut Vec<Diagnostic>,
    ) {
        let Some((l, ri, _)) = self.scan.resolve_qualifier(level, branch, q, self.dialect) else {
            let special = ["excluded", "new", "old"]
                .iter()
                .any(|w| eq_fold(w, &q.value));
            if special || self.res.is_schema_name(q) || self.chain_has_unnamed(level, branch) {
                return;
            }
            out.push(diag(
                &q.span,
                DiagnosticCode::UnknownQualifier,
                format!("unknown table or alias \"{}\"", q.value),
            ));
            return;
        };
        if is_system_column(self.dialect, &c.value) {
            return;
        }
        let cols = cache
            .get(l)
            .and_then(Option::as_ref)
            .and_then(|v| v.get(ri))
            .copied()
            .unwrap_or(Cols::Unknown);
        if matches!(cols, Cols::Unknown) || self.res.find_column(cols, c).is_some() {
            return;
        }
        out.push(unknown_column(c, Some(q)));
    }

    /// Whether a level in the chain has a from-item that cannot be addressed by name.
    fn chain_has_unnamed(&self, level: usize, branch: usize) -> bool {
        let mut next = Some((level, branch));
        while let Some((l, b)) = next {
            if self.scan.opaque(l)
                || self
                    .scan
                    .branch_relations(l, b)
                    .any(|(_, r)| r.visible_name().is_none())
            {
                return true;
            }
            next = self.scan.outer(l);
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn schema_qualified(
        &mut self,
        s: &Ident,
        t: &Ident,
        c: &Ident,
        level: usize,
        branch: usize,
        cache: &[Option<Vec<Cols<'s>>>],
        out: &mut Vec<Diagnostic>,
    ) {
        // `alias.column.field`: a field of a composite column, not a schema.
        if self
            .scan
            .resolve_qualifier(level, branch, s, self.dialect)
            .is_some()
        {
            return;
        }
        let mut found = None;
        let mut next = Some((level, branch));
        while let Some((l, b)) = next {
            for (ri, rel) in self.scan.branch_relations(l, b) {
                if let RelationSource::Table { schema: rs, name } = &rel.source
                    && idents_match(self.dialect, name, t)
                {
                    match rs {
                        Some(rs) if idents_match(self.dialect, rs, s) => found = Some((l, ri)),
                        // Written without a schema (`FROM users`): the search path decides which
                        // schema it is, so `public.users.id` is valid and not judged further.
                        None => return,
                        Some(_) => {}
                    }
                }
            }
            if found.is_some() {
                break;
            }
            next = self.scan.outer(l);
        }
        let Some((l, ri)) = found else {
            if !self.chain_has_unnamed(level, branch) {
                out.push(diag(
                    &t.span,
                    DiagnosticCode::UnknownQualifier,
                    format!("unknown table or alias \"{}\"", t.value),
                ));
            }
            return;
        };
        let cols = cache
            .get(l)
            .and_then(Option::as_ref)
            .and_then(|v| v.get(ri))
            .copied()
            .unwrap_or(Cols::Unknown);
        if matches!(cols, Cols::Unknown)
            || self.res.find_column(cols, c).is_some()
            || is_system_column(self.dialect, &c.value)
        {
            return;
        }
        out.push(unknown_column(c, Some(t)));
    }
}

fn diag(range: &Range<usize>, code: DiagnosticCode, message: String) -> Diagnostic {
    let severity = if code == DiagnosticCode::AmbiguousColumn {
        Severity::Warning
    } else {
        Severity::Error
    };
    Diagnostic {
        range: range.clone(),
        severity,
        code,
        message,
    }
}

fn unknown_column(c: &Ident, qualifier: Option<&Ident>) -> Diagnostic {
    let message = match qualifier {
        Some(q) => format!("unknown column \"{}\" in \"{}\"", c.value, q.value),
        None => format!("unknown column \"{}\"", c.value),
    };
    diag(&c.span, DiagnosticCode::UnknownColumn, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySchema;
    use sqler_core::{ColumnInfo, DataType, ObjectKind};

    fn schema() -> MemorySchema {
        let mut s = MemorySchema::new(["public"]);
        let cols = |n: &[&str]| {
            Some(
                n.iter()
                    .map(|c| ColumnInfo::new(*c, DataType::Integer))
                    .collect(),
            )
        };
        s.add_table("public", "users", ObjectKind::Table, cols(&["id", "name"]));
        s.add_table(
            "public",
            "orders",
            ObjectKind::Table,
            cols(&["id", "user_id"]),
        );
        s
    }

    fn codes(sql: &str) -> Vec<(DiagnosticCode, String)> {
        diagnose(sql, Dialect::Postgres, &schema())
            .into_iter()
            .map(|d| (d.code, sql[d.range].to_owned()))
            .collect()
    }

    #[test]
    fn basics() {
        use DiagnosticCode::*;
        assert_eq!(
            codes("SELECT u.nme FROM users u"),
            [(UnknownColumn, "nme".to_owned())]
        );
        assert_eq!(
            codes("SELECT x.id FROM users u"),
            [(UnknownQualifier, "x".to_owned())]
        );
        assert_eq!(
            codes("SELECT 1 FROM usrs"),
            [(UnknownRelation, "usrs".to_owned())]
        );
        assert_eq!(
            codes("SELECT 1 FROM nope.users"),
            [(UnknownSchema, "nope".to_owned())]
        );
        assert_eq!(
            codes("SELECT id FROM users u JOIN orders o ON o.user_id = u.id"),
            [(AmbiguousColumn, "id".to_owned())]
        );
        assert_eq!(codes("SELECT u.id, name FROM users u"), []);
    }
}
