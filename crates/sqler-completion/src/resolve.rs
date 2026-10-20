// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Resolving typed names against a [`SchemaSource`]: relations, columns and foreign keys.
//!
//! Diagnostics use strict dialect matching and a per-call name index (a script with thousands of
//! statements would otherwise scan every relation name for every reference); hover uses lenient
//! matching (a strict match first, a case-insensitive one as fallback) and no index.

use std::collections::HashMap;

use sqler_core::{ColumnInfo, ForeignKey};
use sqler_sql::Dialect;

use crate::dialect_data::{
    find_stored_by, fold_hash, idents_match, is_system_relation, is_system_schema, stored_eq_strict,
};
use crate::lexer::{TokKind, Token};
use crate::scope::{
    Clause, Cte, Ident, RelationSource, Scan, ScopeRelation, ident_from_token, is_scan_keyword,
};
use crate::source::{Loadable, Relation, SchemaSource};

/// The outcome of looking up a (possibly schema-qualified) relation name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelLookup<'a> {
    /// The relation exists.
    Found(Relation<'a>),
    /// The schema is not among the known schemas.
    UnknownSchema,
    /// The schema (if any) is known, the relation is not.
    Unknown,
    /// The source cannot tell: relations not loaded, system names, no schemas at all.
    NotKnowable,
}

/// The columns of a relation as far as they are known.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cols<'a> {
    /// A table or view with full column information.
    Info(&'a [ColumnInfo]),
    /// A CTE or subquery: names only.
    Names(&'a [String]),
    /// Not known: not loaded, unresolved, created in the text, or not nameable.
    Unknown,
}

/// A column found in [`Cols`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum ColHit<'a> {
    Info(&'a ColumnInfo),
    Name(&'a str),
}

/// The relations of one schema, sorted by the hash of the folded name.
struct NameIndex<'a> {
    entries: Vec<(u64, Relation<'a>)>,
}

impl<'a> NameIndex<'a> {
    fn build(rels: impl Iterator<Item = Relation<'a>>) -> Self {
        let mut entries: Vec<(u64, Relation<'a>)> = rels.map(|r| (fold_hash(r.name), r)).collect();
        entries.sort_by_key(|e| e.0);
        Self { entries }
    }

    fn bucket(&self, hash: u64) -> impl Iterator<Item = &Relation<'a>> {
        let from = self.entries.partition_point(|e| e.0 < hash);
        self.entries
            .get(from..)
            .unwrap_or(&[])
            .iter()
            .take_while(move |e| e.0 == hash)
            .map(|e| &e.1)
    }
}

/// The name index of one schema, built on first use.
enum Slot<'a> {
    Empty,
    /// The relations of the schema are not loaded.
    NotLoaded,
    Ready(NameIndex<'a>),
}

impl<'a> Slot<'a> {
    fn fill(
        &mut self,
        source: &'a dyn SchemaSource,
        schema: Option<&str>,
    ) -> Option<&NameIndex<'a>> {
        if matches!(self, Slot::Empty) {
            *self = match source.relations(schema) {
                Loadable::Loaded(rels) => Slot::Ready(NameIndex::build(rels)),
                Loadable::NotLoaded => Slot::NotLoaded,
            };
        }
        match self {
            Slot::Ready(index) => Some(index),
            Slot::Empty | Slot::NotLoaded => None,
        }
    }
}

/// Maps typed names to stored names.
pub(crate) struct Resolver<'a> {
    schema: &'a dyn SchemaSource,
    dialect: Dialect,
    strict: bool,
    indexed: bool,
    schemas: Option<Vec<&'a str>>,
    unqualified: Slot<'a>,
    qualified: HashMap<&'a str, Slot<'a>>,
    /// Names created by `CREATE TABLE|VIEW` in the text: never reported, columns unknown.
    pub locals: Vec<Ident>,
    /// Schemas created or attached in the text: never reported.
    pub local_schemas: Vec<Ident>,
}

impl<'a> Resolver<'a> {
    /// A resolver; `strict` selects exact dialect matching, `indexed` the per-call name index.
    pub(crate) fn new(
        schema: &'a dyn SchemaSource,
        dialect: Dialect,
        strict: bool,
        indexed: bool,
    ) -> Self {
        Self {
            schema,
            dialect,
            strict,
            indexed,
            schemas: None,
            unqualified: Slot::Empty,
            qualified: HashMap::new(),
            locals: Vec::new(),
            local_schemas: Vec::new(),
        }
    }

    pub(crate) fn schema_source(&self) -> &'a dyn SchemaSource {
        self.schema
    }

    /// Whether `name` is a table or view created in the text.
    pub(crate) fn is_local(&self, name: &Ident) -> bool {
        self.locals
            .iter()
            .any(|l| idents_match(self.dialect, l, name))
    }

    fn is_local_schema(&self, id: &Ident) -> bool {
        self.local_schemas
            .iter()
            .any(|l| idents_match(self.dialect, l, id))
    }

    fn schema_names(&mut self) -> &[&'a str] {
        let schema = self.schema;
        self.schemas
            .get_or_insert_with(|| schema.schemas().collect())
    }

    /// The stored name of a typed schema.
    pub(crate) fn find_schema(&mut self, id: &Ident) -> Option<&'a str> {
        let dialect = self.dialect;
        let strict = self.strict;
        let names = self.schema_names();
        if strict {
            names
                .iter()
                .copied()
                .find(|n| stored_eq_strict(dialect, id, n))
        } else {
            find_stored_by(dialect, id, names.iter().copied(), |n| n)
        }
    }

    /// Whether `id` names a known schema (any case), for diagnostics that must not guess.
    pub(crate) fn is_schema_name(&mut self, id: &Ident) -> bool {
        if self.is_local_schema(id) {
            return true;
        }
        let names = self.schema_names();
        names
            .iter()
            .any(|n| crate::dialect_data::eq_fold(n, &id.value))
    }

    /// Looks up `schema.name` (or `name` on the search path).
    pub(crate) fn relation(&mut self, schema: Option<&Ident>, name: &Ident) -> RelLookup<'a> {
        if self.is_local(name) {
            return RelLookup::NotKnowable;
        }
        let mut stored_schema = None;
        if let Some(sch) = schema {
            if self.schema_names().is_empty()
                || is_system_schema(self.dialect, &sch.value)
                || self.is_local_schema(sch)
            {
                return RelLookup::NotKnowable;
            }
            match self.find_schema(sch) {
                Some(n) => stored_schema = Some(n),
                None => return RelLookup::UnknownSchema,
            }
        } else if is_system_relation(self.dialect, &name.value)
            || self.schema.search_path().is_empty()
        {
            return RelLookup::NotKnowable;
        }
        let found = if self.indexed {
            self.indexed_lookup(stored_schema, name)
        } else {
            self.linear_lookup(stored_schema, name)
        };
        match found {
            Lookup::NotLoaded => RelLookup::NotKnowable,
            Lookup::Missing => RelLookup::Unknown,
            Lookup::Hit(r) => {
                RelLookup::Found(Relation::new(r.schema.or(stored_schema), r.name, r.kind))
            }
        }
    }

    fn linear_lookup(&self, stored_schema: Option<&'a str>, name: &Ident) -> Lookup<'a> {
        let Loadable::Loaded(rels) = self.schema.relations(stored_schema) else {
            return Lookup::NotLoaded;
        };
        let hit = if self.strict {
            let mut rels = rels;
            rels.find(|r| stored_eq_strict(self.dialect, name, r.name))
        } else {
            find_stored_by(self.dialect, name, rels, |r| r.name)
        };
        hit.map_or(Lookup::Missing, Lookup::Hit)
    }

    fn indexed_lookup(&mut self, stored_schema: Option<&'a str>, name: &Ident) -> Lookup<'a> {
        let source = self.schema;
        let slot = match stored_schema {
            Some(sch) => self.qualified.entry(sch).or_insert(Slot::Empty),
            None => &mut self.unqualified,
        };
        let Some(index) = slot.fill(source, stored_schema) else {
            return Lookup::NotLoaded;
        };
        let dialect = self.dialect;
        let hash = fold_hash(&name.value);
        let mut loose = None;
        for r in index.bucket(hash) {
            if stored_eq_strict(dialect, name, r.name) {
                return Lookup::Hit(*r);
            }
            if loose.is_none() && crate::dialect_data::eq_fold(r.name, &name.value) {
                loose = Some(*r);
            }
        }
        match loose {
            Some(r) if !self.strict => Lookup::Hit(r),
            _ => Lookup::Missing,
        }
    }

    /// The columns of a relation in scope at `level` of `scan`.
    pub(crate) fn columns<'x>(
        &mut self,
        rel: &'x ScopeRelation,
        scan: &'x Scan,
        level: usize,
    ) -> Cols<'x>
    where
        'a: 'x,
    {
        match &rel.source {
            RelationSource::Table { schema, name } => {
                if schema.is_none()
                    && let Some(cte) = scan.find_cte(level, name, self.dialect)
                {
                    return cte_cols(cte, scan);
                }
                match self.relation(schema.as_ref(), name) {
                    RelLookup::Found(r) => match self.schema.columns(r.schema, r.name) {
                        Loadable::Loaded(c) => Cols::Info(c),
                        Loadable::NotLoaded => Cols::Unknown,
                    },
                    _ => Cols::Unknown,
                }
            }
            RelationSource::Cte { name } => scan
                .find_cte(level, name, self.dialect)
                .map_or(Cols::Unknown, |c| cte_cols(c, scan)),
            RelationSource::Subquery { columns } => {
                if scan.is_incomplete(rel.span.start) {
                    Cols::Unknown
                } else {
                    Cols::Names(columns)
                }
            }
        }
    }

    /// The column `id` in `cols`, if present.
    pub(crate) fn find_column<'x>(&self, cols: Cols<'x>, id: &Ident) -> Option<ColHit<'x>> {
        let dialect = self.dialect;
        match cols {
            Cols::Info(infos) => if self.strict {
                infos
                    .iter()
                    .find(|c| stored_eq_strict(dialect, id, &c.name))
            } else {
                find_stored_by(dialect, id, infos.iter(), |c| &c.name)
            }
            .map(ColHit::Info),
            Cols::Names(names) => if self.strict {
                names.iter().find(|n| stored_eq_strict(dialect, id, n))
            } else {
                find_stored_by(dialect, id, names.iter(), |n| n.as_str())
            }
            .map(|n| ColHit::Name(n.as_str())),
            Cols::Unknown => None,
        }
    }

    /// The foreign key that `column` takes part in: a single-column key first, else the first.
    pub(crate) fn foreign_key(
        &self,
        schema: Option<&str>,
        relation: &str,
        column: &str,
    ) -> Option<ForeignKey> {
        let Loadable::Loaded(fks) = self.schema.foreign_keys(schema, relation) else {
            return None;
        };
        let mut with_column = fks.iter().filter(|f| f.columns.iter().any(|c| c == column));
        let first = with_column.next()?;
        if first.columns.len() == 1 {
            return Some(first.clone());
        }
        with_column
            .find(|f| f.columns.len() == 1)
            .or(Some(first))
            .cloned()
    }
}

fn cte_cols<'x>(cte: &'x Cte, scan: &Scan) -> Cols<'x> {
    if scan.is_incomplete(cte.span.start) {
        Cols::Unknown
    } else {
        Cols::Names(&cte.columns)
    }
}

/// A dotted run of names: `a`, `a.b`, `a.b.c`.
#[derive(Debug)]
pub(crate) struct Chain {
    /// The names, outermost first.
    pub parts: Vec<Ident>,
    /// The index of each name's token.
    pub toks: Vec<usize>,
    /// The index of the token after the chain.
    pub next: usize,
    /// The chain ends in a dot that no name follows (`t.*`, `t.` while typing).
    pub dangling: bool,
}

/// Reads the chain whose first name is the token `start`.
pub(crate) fn read_chain(text: &str, toks: &[Token], start: usize) -> Chain {
    let mut chain = Chain {
        parts: Vec::new(),
        toks: Vec::new(),
        next: start,
        dangling: false,
    };
    let mut i = start;
    while let Some(t) = toks.get(i) {
        if !matches!(t.kind, TokKind::Word | TokKind::Quoted) {
            break;
        }
        chain.parts.push(ident_from_token(text, t));
        chain.toks.push(i);
        i += 1;
        match toks.get(i) {
            Some(d) if d.is_punct(text, '.') => {
                i += 1;
                chain.dangling = true;
            }
            _ => {
                chain.dangling = false;
                break;
            }
        }
        // A keyword after a dot with a gap (`u. FROM t`) is the next clause, not a name.
        let gap_keyword = |n: &Token| {
            n.kind == TokKind::Word
                && is_scan_keyword(n.text(text))
                && toks.get(i - 1).is_some_and(|d| d.end != n.start)
        };
        if !toks
            .get(i)
            .is_some_and(|n| matches!(n.kind, TokKind::Word | TokKind::Quoted) && !gap_keyword(n))
        {
            break;
        }
        chain.dangling = false;
    }
    chain.next = i;
    chain
}

/// Whether a token in this clause can be a column reference.
pub(crate) fn is_expr_clause(clause: Clause) -> bool {
    matches!(
        clause,
        Clause::Select
            | Clause::Where
            | Clause::On
            | Clause::Group
            | Clause::Order
            | Clause::Having
            | Clause::Set
            | Clause::Returning
    )
}

enum Lookup<'a> {
    NotLoaded,
    Missing,
    Hit(Relation<'a>),
}

#[cfg(test)]
mod tests {
    use sqler_core::{DataType, ObjectKind};

    use super::*;
    use crate::MemorySchema;
    use crate::lexer::{TokKind, lex};
    use crate::scope::scan_all;

    fn id(v: &str, quoted: bool) -> Ident {
        Ident::new(v, quoted)
    }

    fn schema() -> MemorySchema {
        let mut s = MemorySchema::new(["public"]);
        let cols = |n: &[&str]| {
            Some(
                n.iter()
                    .map(|c| ColumnInfo::new(*c, DataType::Integer))
                    .collect(),
            )
        };
        s.add_table("public", "users", ObjectKind::Table, cols(&["id", "Name"]));
        s.add_table("public", "Users", ObjectKind::Table, cols(&["other"]));
        s.add_table("public", "pending", ObjectKind::Table, None);
        s.add_table("sales", "orders", ObjectKind::Table, cols(&["id"]));
        s
    }

    fn lookups(indexed: bool, strict: bool) -> Vec<String> {
        let s = schema();
        let mut r = Resolver::new(&s, Dialect::Postgres, strict, indexed);
        let cases: [(Option<Ident>, Ident); 8] = [
            (None, id("users", false)),
            (None, id("USERS", false)),
            (None, id("Users", true)),
            (None, id("uSeRs", true)),
            (None, id("nope", false)),
            (Some(id("sales", false)), id("orders", false)),
            (Some(id("nope", false)), id("orders", false)),
            (Some(id("sales", false)), id("nope", false)),
        ];
        cases
            .iter()
            .map(|(sc, n)| match r.relation(sc.as_ref(), n) {
                RelLookup::Found(f) => format!("{:?}.{}", f.schema, f.name),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn strict_and_lenient_lookups_agree_with_and_without_index() {
        for indexed in [false, true] {
            let strict = lookups(indexed, true);
            assert_eq!(strict[0], "Some(\"public\").users");
            assert_eq!(strict[1], "Some(\"public\").users");
            assert_eq!(strict[2], "Some(\"public\").Users");
            assert_eq!(strict[3], "Unknown", "strict: case differs");
            assert_eq!(strict[4], "Unknown");
            assert_eq!(strict[5], "Some(\"sales\").orders");
            assert_eq!(strict[6], "UnknownSchema");
            assert_eq!(strict[7], "Unknown");
            let lenient = lookups(indexed, false);
            assert_eq!(lenient[3], "Some(\"public\").users", "lenient fallback");
            assert_eq!(lenient[..3], strict[..3]);
        }
    }

    #[test]
    fn dialect_case_rules() {
        let s = schema();
        for d in [Dialect::MySql, Dialect::Sqlite] {
            let mut r = Resolver::new(&s, d, true, true);
            assert!(matches!(
                r.relation(None, &id("PENDING", false)),
                RelLookup::Found(_)
            ));
            assert!(matches!(
                r.relation(Some(&id("SALES", false)), &id("ORDERS", true)),
                RelLookup::Found(_)
            ));
        }
    }

    #[test]
    fn not_knowable_cases() {
        let mut s = schema();
        s.mark_relations_not_loaded("sales");
        let mut r = Resolver::new(&s, Dialect::Postgres, true, true);
        let cases = [
            (Some(id("sales", false)), id("orders", false)),
            (Some(id("information_schema", false)), id("tables", false)),
            (Some(id("pg_catalog", false)), id("pg_class", false)),
            (None, id("pg_class", false)),
        ];
        for (sc, n) in &cases {
            assert_eq!(r.relation(sc.as_ref(), n), RelLookup::NotKnowable);
        }
        r.locals.push(id("tmp", false));
        assert_eq!(r.relation(None, &id("TMP", false)), RelLookup::NotKnowable);
        // No schemas at all, or an empty search path: nothing can be said.
        let empty = MemorySchema::default();
        let mut r = Resolver::new(&empty, Dialect::Postgres, true, true);
        assert_eq!(r.relation(None, &id("x", false)), RelLookup::NotKnowable);
        assert_eq!(
            r.relation(Some(&id("s", false)), &id("x", false)),
            RelLookup::NotKnowable
        );
        // The search path itself is not loaded.
        let mut s = schema();
        s.mark_relations_not_loaded("public");
        let mut r = Resolver::new(&s, Dialect::Postgres, true, false);
        assert_eq!(
            r.relation(None, &id("users", false)),
            RelLookup::NotKnowable
        );
    }

    #[test]
    fn columns_of_tables_ctes_and_subqueries() {
        let s = schema();
        let text = "WITH c AS (SELECT id AS k FROM users), d AS (SELECT * FROM users) \
                    SELECT 1 FROM users u, pending p, c, d, (SELECT id AS x FROM users) s, \
                    (SELECT * FROM users) t, nope n";
        let toks: Vec<_> = lex(text, Dialect::Postgres)
            .into_iter()
            .filter(|t| t.kind != TokKind::Comment)
            .collect();
        let scan = scan_all(text, &toks, Dialect::Postgres);
        let rels = scan.relations(0);
        let mut r = Resolver::new(&s, Dialect::Postgres, true, true);
        let kinds: Vec<&str> = rels
            .iter()
            .map(|rel| match r.columns(rel, &scan, 0) {
                Cols::Info(_) => "info",
                Cols::Names(_) => "names",
                Cols::Unknown => "unknown",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "info", "unknown", "names", "unknown", "names", "unknown", "unknown"
            ]
        );
        let c = r.columns(&rels[0], &scan, 0);
        assert!(
            r.find_column(c, &id("NAME", false)).is_none(),
            "strict case"
        );
        assert!(r.find_column(c, &id("Name", true)).is_some());
        let lenient = Resolver::new(&s, Dialect::Postgres, false, false);
        assert!(lenient.find_column(c, &id("name", false)).is_some());
        let names = r.columns(&rels[2], &scan, 0);
        assert!(r.find_column(names, &id("k", false)).is_some());
    }

    #[test]
    fn foreign_key_prefers_single_column_keys() {
        let mut s = schema();
        let two = ForeignKey::new(
            vec!["a".into(), "b".into()],
            "x",
            vec!["p".into(), "q".into()],
        );
        let one = ForeignKey::new(vec!["b".into()], "y", vec!["id".into()]);
        s.add_foreign_key("public", "users", two.clone());
        s.add_foreign_key("public", "users", one.clone());
        let r = Resolver::new(&s, Dialect::Postgres, true, false);
        assert_eq!(r.foreign_key(Some("public"), "users", "a"), Some(two));
        assert_eq!(r.foreign_key(Some("public"), "users", "b"), Some(one));
        assert_eq!(r.foreign_key(Some("public"), "users", "zz"), None);
        assert_eq!(r.foreign_key(Some("public"), "nope", "a"), None);
    }
}
