// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Hover: what is the identifier under the cursor. Uses the same lexer, scope scanner and
//! alias rules as completion, with lenient name matching.

use std::ops::{ControlFlow, Range};

use sqler_core::{ColumnInfo, ForeignKey, ObjectKind};
use sqler_sql::Dialect;

use crate::context::clamp_offset;
use crate::lexer::{TokKind, Token, lex};
use crate::resolve::{ColHit, Cols, RelLookup, Resolver, is_expr_clause, read_chain};
use crate::scope::{Ident, LevelKind, RelationSource, Scan, ScopeRelation, scan_all};
use crate::source::SchemaSource;

/// What to hover: text, cursor and dialect.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct HoverRequest<'a> {
    /// The whole editor text.
    pub text: &'a str,
    /// The cursor as a UTF-8 byte offset; clamped to the text and floored to a char boundary.
    pub offset: usize,
    /// The SQL dialect.
    pub dialect: Dialect,
}

impl<'a> HoverRequest<'a> {
    /// A hover request.
    #[must_use]
    pub fn new(text: &'a str, offset: usize, dialect: Dialect) -> Self {
        Self {
            text,
            offset,
            dialect,
        }
    }
}

/// What kind of relation a [`RelationInfo`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RelationKind {
    /// A base table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A common table expression.
    Cte,
    /// A subquery or table function with an alias.
    Subquery,
}

/// A relation, with its names as the schema stores them (a CTE or subquery: as written).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RelationInfo {
    /// The schema that holds it, if known.
    pub schema: Option<String>,
    /// The relation name.
    pub name: String,
    /// What it is.
    pub kind: RelationKind,
}

impl RelationInfo {
    /// Builds a relation description.
    #[must_use]
    pub fn new(schema: Option<&str>, name: impl Into<String>, kind: RelationKind) -> Self {
        Self {
            schema: schema.map(str::to_owned),
            name: name.into(),
            kind,
        }
    }
}

/// What the identifier under the cursor denotes.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HoverTarget {
    /// A table, view, materialized view, CTE or subquery alias (also hovered through its alias).
    Relation(RelationInfo),
    /// A column of a table or view: type, nullability and, if the column is part of one, the
    /// foreign key.
    Column {
        /// The relation that holds it.
        relation: RelationInfo,
        /// The column as the schema stores it.
        info: ColumnInfo,
        /// A foreign key the column takes part in.
        references: Option<ForeignKey>,
    },
    /// A column of a CTE or subquery: only the name is known.
    DerivedColumn {
        /// The CTE or subquery.
        relation: RelationInfo,
        /// The column name.
        name: String,
    },
}

/// The answer to a [`HoverRequest`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Hover {
    /// The byte range of the identifier, quotes included.
    pub range: Range<usize>,
    /// What it is.
    pub target: HoverTarget,
}

/// Describes the identifier at the cursor of `req`, reading names from `schema`.
///
/// Pure and total: any text and any offset give an answer. `None` for keywords, literals,
/// comments, punctuation, schema names, and names that cannot be resolved (including columns
/// of relations whose columns are not loaded).
#[must_use]
pub fn hover(req: &HoverRequest<'_>, schema: &dyn SchemaSource) -> Option<Hover> {
    let text = req.text;
    let dialect = req.dialect;
    let off = clamp_offset(text, req.offset);
    let toks = lex(text, dialect);
    let is_name = |t: &Token| matches!(t.kind, TokKind::Word | TokKind::Quoted);
    let idx = toks.partition_point(|t| t.end <= off);
    let hit = toks
        .get(idx)
        .filter(|t| t.start <= off && is_name(t))
        .or_else(|| {
            idx.checked_sub(1)
                .and_then(|p| toks.get(p))
                .filter(|t| t.end == off && is_name(t))
        })?;
    let tok_pos = toks.iter().position(|t| t.start == hit.start)?;

    // The statement around the token.
    let is_semi = |t: &Token| t.is_punct(text, ';');
    let lo = toks
        .get(..tok_pos)
        .and_then(|h| h.iter().rposition(is_semi))
        .map_or(0, |p| p + 1);
    let hi = toks
        .get(tok_pos..)
        .and_then(|t| t.iter().position(is_semi))
        .map_or(toks.len(), |p| tok_pos + p);
    let stmt: Vec<Token> = toks
        .get(lo..hi)?
        .iter()
        .copied()
        .filter(|t| t.kind != TokKind::Comment)
        .collect();
    let pos = stmt.iter().position(|t| t.start == hit.start)?;

    let scan = scan_all(text, &stmt, dialect);
    let mut h = Hovering {
        text,
        toks: &stmt,
        scan: &scan,
        res: Resolver::new(schema, dialect, false, false),
        dialect,
    };
    let target = h.target(pos)?;
    Some(Hover {
        range: hit.start..hit.end,
        target,
    })
}

struct Hovering<'a> {
    text: &'a str,
    toks: &'a [Token],
    scan: &'a Scan,
    res: Resolver<'a>,
    dialect: Dialect,
}

impl<'a> Hovering<'a> {
    fn target(&mut self, pos: usize) -> Option<HoverTarget> {
        let tok = *self.toks.get(pos)?;
        let span = tok.start..tok.end;
        if let ControlFlow::Break(t) = self.relation_token(&span) {
            return t;
        }
        let snap = *self.scan.snaps.get(pos)?;
        if snap.kind == LevelKind::InsertCols {
            let parent = self.scan.parent(snap.level)?;
            let rel = self.scan.relations(parent).get(self.scan.target(parent)?)?;
            let id = crate::scope::ident_from_token(self.text, &tok);
            return self.column_of(rel, parent, &id);
        }
        let clause = self.scan.effective_clause(&snap);
        if !is_expr_clause(clause) {
            return None;
        }
        // The chain this token belongs to.
        let mut start = pos;
        while start >= 2
            && self.toks[start - 1].is_punct(self.text, '.')
            && matches!(self.toks[start - 2].kind, TokKind::Word | TokKind::Quoted)
        {
            start -= 2;
        }
        let chain = read_chain(self.text, self.toks, start);
        if chain.dangling && chain.toks.last() == Some(&pos) {
            // Cannot happen (a dangling chain ends in a dot), kept for totality.
            return None;
        }
        let k = chain.toks.iter().position(|&t| t == pos)?;
        if self
            .toks
            .get(chain.next)
            .is_some_and(|t| t.is_punct(self.text, '('))
        {
            return None;
        }
        if start > 0 && self.preceded_by_skip(start) {
            return None;
        }
        let level = self.scan.query_level(snap.level);
        let branch = snap.branch;
        match chain.parts.as_slice() {
            [id] => self.unqualified(id, level, branch),
            [qual, col] => {
                let (at, _, rel) =
                    self.scan
                        .resolve_qualifier(level, branch, qual, self.dialect)?;
                if k == 0 {
                    self.relation_target(rel, at)
                } else {
                    self.column_of(rel, at, col)
                }
            }
            [schema, table, col] => {
                if k == 0 {
                    return None;
                }
                let found = self.table_in_scope(level, branch, schema, table);
                match (found, k) {
                    (Some((at, rel)), 1) => self.relation_target(rel, at),
                    (Some((at, rel)), _) => self.column_of(rel, at, col),
                    (None, 1) => self
                        .table_info(Some(schema), table)
                        .map(HoverTarget::Relation),
                    (None, _) => self.table_column(schema, table, col),
                }
            }
            _ => None,
        }
    }

    fn preceded_by_skip(&self, start: usize) -> bool {
        let Some(prev) = start.checked_sub(1).and_then(|p| self.toks.get(p)) else {
            return false;
        };
        prev.is_punct(self.text, '.')
            || prev.is_punct(self.text, ':')
            || (prev.kind == TokKind::Word && prev.text(self.text).eq_ignore_ascii_case("as"))
    }

    /// A token that is a CTE name, or the name or alias of a relation: `Break` with the answer.
    /// `Continue` when the token is none of those.
    fn relation_token(&mut self, span: &Range<usize>) -> ControlFlow<Option<HoverTarget>> {
        let tree = self.scan;
        for level in 0..tree.level_count() {
            if let Some(cte) = tree.ctes(level).iter().find(|c| &c.name.span == span) {
                return ControlFlow::Break(Some(HoverTarget::Relation(RelationInfo::new(
                    None,
                    cte.name.value.clone(),
                    RelationKind::Cte,
                ))));
            }
            for rel in tree.relations(level) {
                if rel.alias.as_ref().is_some_and(|a| &a.span == span) {
                    return ControlFlow::Break(self.relation_target(rel, level));
                }
                match &rel.source {
                    RelationSource::Table { schema, name } => {
                        if schema.as_ref().is_some_and(|sch| &sch.span == span) {
                            return ControlFlow::Break(None);
                        }
                        if &name.span == span {
                            return ControlFlow::Break(self.relation_target(rel, level));
                        }
                    }
                    RelationSource::Cte { name } if &name.span == span => {
                        return ControlFlow::Break(self.relation_target(rel, level));
                    }
                    _ => {}
                }
            }
        }
        ControlFlow::Continue(())
    }

    fn table_info(&mut self, schema: Option<&Ident>, name: &Ident) -> Option<RelationInfo> {
        match self.res.relation(schema, name) {
            RelLookup::Found(r) => Some(RelationInfo {
                schema: r.schema.map(str::to_owned),
                name: r.name.to_owned(),
                kind: match r.kind {
                    ObjectKind::View => RelationKind::View,
                    ObjectKind::MaterializedView => RelationKind::MaterializedView,
                    _ => RelationKind::Table,
                },
            }),
            _ => None,
        }
    }

    fn relation_info(&mut self, rel: &ScopeRelation, level: usize) -> Option<RelationInfo> {
        match &rel.source {
            RelationSource::Table { schema, name } => {
                if schema.is_none()
                    && let Some(cte) = self.scan.find_cte(level, name, self.dialect)
                {
                    return Some(RelationInfo {
                        schema: None,
                        name: cte.name.value.clone(),
                        kind: RelationKind::Cte,
                    });
                }
                self.table_info(schema.as_ref(), name)
            }
            RelationSource::Cte { name } => Some(RelationInfo {
                schema: None,
                name: self
                    .scan
                    .find_cte(level, name, self.dialect)
                    .map_or_else(|| name.value.clone(), |c| c.name.value.clone()),
                kind: RelationKind::Cte,
            }),
            RelationSource::Subquery { .. } => rel.alias.as_ref().map(|a| RelationInfo {
                schema: None,
                name: a.value.clone(),
                kind: RelationKind::Subquery,
            }),
        }
    }

    fn relation_target(&mut self, rel: &ScopeRelation, level: usize) -> Option<HoverTarget> {
        self.relation_info(rel, level).map(HoverTarget::Relation)
    }

    /// The column `id` of `rel`, or `None` when the relation's columns are not known.
    fn column_of(&mut self, rel: &ScopeRelation, level: usize, id: &Ident) -> Option<HoverTarget> {
        let cols = self.res.columns(rel, self.scan, level);
        let info = self.relation_info(rel, level)?;
        self.column_target(cols, info, id)
    }

    fn column_target(&self, cols: Cols<'_>, info: RelationInfo, id: &Ident) -> Option<HoverTarget> {
        match self.res.find_column(cols, id)? {
            ColHit::Info(ci) => {
                let references = if info.kind == RelationKind::Table
                    || info.kind == RelationKind::View
                    || info.kind == RelationKind::MaterializedView
                {
                    self.res
                        .foreign_key(info.schema.as_deref(), &info.name, &ci.name)
                } else {
                    None
                };
                Some(HoverTarget::Column {
                    relation: info,
                    info: ci.clone(),
                    references,
                })
            }
            ColHit::Name(n) => Some(HoverTarget::DerivedColumn {
                relation: info,
                name: n.to_owned(),
            }),
        }
    }

    /// `schema.table.column` where the table is not in the `FROM` clause.
    fn table_column(&mut self, s: &Ident, t: &Ident, c: &Ident) -> Option<HoverTarget> {
        let info = self.table_info(Some(s), t)?;
        let schema = self.res.schema_source();
        let stored = info.schema.as_deref();
        let crate::source::Loadable::Loaded(infos) = schema.columns(stored, &info.name) else {
            return None;
        };
        self.column_target(Cols::Info(infos), info, c)
    }

    /// A table relation in scope that `s.t` names.
    fn table_in_scope(
        &self,
        level: usize,
        branch: usize,
        s: &Ident,
        t: &Ident,
    ) -> Option<(usize, &'a ScopeRelation)> {
        let scan = self.scan;
        let mut next = Some((level, branch));
        while let Some((l, b)) = next {
            for (_, rel) in scan.branch_relations(l, b) {
                if let RelationSource::Table {
                    schema: Some(rs),
                    name,
                } = &rel.source
                    && crate::dialect_data::idents_match(self.dialect, rs, s)
                    && crate::dialect_data::idents_match(self.dialect, name, t)
                {
                    return Some((l, rel));
                }
            }
            next = scan.outer(l);
        }
        None
    }

    /// A bare column: the first relation in scope (own level first) that holds it.
    fn unqualified(&mut self, id: &Ident, level: usize, branch: usize) -> Option<HoverTarget> {
        let scan = self.scan;
        let mut next = Some((level, branch));
        while let Some((l, b)) = next {
            for (_, rel) in scan.branch_relations(l, b) {
                let cols = self.res.columns(rel, scan, l);
                if self.res.find_column(cols, id).is_some() {
                    return self.column_of(rel, l, id);
                }
            }
            next = scan.outer(l);
        }
        None
    }
}
