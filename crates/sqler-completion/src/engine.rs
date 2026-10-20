// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The completion entry point: candidates from the scope, the schema and the dialect tables,
//! filtered and ranked.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use sqler_core::{ColumnInfo, ObjectKind};
use sqler_sql::Dialect;

use crate::context::{Analysis, ContextKind, analyze, clamp_offset};
use crate::dialect_data::{
    Func, RELATION_KEYWORDS, SNIPPETS, Snippet, TAIL_KEYWORDS, apply_case, find_stored,
    find_stored_by, fold, functions, idents_match, keywords, quote, statement_keywords, text_name,
};
use crate::rank::{Class, FUZZY, Matcher, cmp_labels};
use crate::scope::{Ident, RelationSource, ScopeRelation};
use crate::source::{Loadable, SchemaSource};

/// The default maximum number of returned items.
const DEFAULT_LIMIT: usize = 200;

/// What to complete: text, cursor and dialect.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CompletionRequest<'a> {
    /// The whole editor text.
    pub text: &'a str,
    /// The cursor as a UTF-8 byte offset; clamped to the text and floored to a char boundary.
    pub offset: usize,
    /// The SQL dialect.
    pub dialect: Dialect,
    /// The maximum number of items returned (default 200).
    pub limit: usize,
}

impl<'a> CompletionRequest<'a> {
    /// A request with the default limit.
    #[must_use]
    pub fn new(text: &'a str, offset: usize, dialect: Dialect) -> Self {
        Self {
            text,
            offset,
            dialect,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// The kind of a completion item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemKind {
    /// A schema.
    Schema,
    /// A table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MaterializedView,
    /// A common table expression.
    Cte,
    /// A relation alias (or a bare relation name in scope).
    Alias,
    /// A column.
    Column,
    /// A built-in function.
    Function,
    /// A keyword.
    Keyword,
    /// A snippet.
    Snippet,
}

/// How `insert_text` is to be read.
///
/// Two states, so this enum is not `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InsertFormat {
    /// Insert the text as it is.
    Plain,
    /// LSP snippet syntax: `$1`, `${2:placeholder}` and the final stop `$0`.
    Snippet,
}

/// One completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompletionItem {
    /// The text shown and matched.
    pub label: String,
    /// What it is.
    pub kind: ItemKind,
    /// The text to insert in place of [`Completions::replace`], quoted when needed.
    pub insert_text: String,
    /// How to read `insert_text`.
    pub format: InsertFormat,
    /// Short extra information: the owner and type of a column, the signature of a function.
    pub detail: Option<String>,
    /// Higher is better; strictly decreasing along `items`.
    pub score: u32,
}

/// Something the schema source has not loaded yet; the caller may fetch it and ask again.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaRequest {
    /// The relations of a schema (`None`: the search path).
    Relations {
        /// The schema, as stored.
        schema: Option<String>,
    },
    /// The columns of a relation.
    Columns {
        /// The schema, as stored.
        schema: Option<String>,
        /// The relation, as stored.
        relation: String,
    },
}

/// The answer to a [`CompletionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Completions {
    /// Best first, at most `limit`.
    pub items: Vec<CompletionItem>,
    /// The byte range of the text that an item replaces.
    pub replace: Range<usize>,
    /// What was completed.
    pub context: ContextKind,
    /// Data that was missing; empty when everything needed was loaded.
    pub requests: Vec<SchemaRequest>,
}

/// Completes at the cursor of `req`, reading names from `schema`.
///
/// Pure and total: any text and any offset give an answer (possibly with no items).
#[must_use]
pub fn complete(req: &CompletionRequest<'_>, schema: &dyn SchemaSource) -> Completions {
    let analysis = analyze(req.text, req.offset, req.dialect);
    let cursor = clamp_offset(req.text, req.offset);
    let mut run = Run::new(req, schema, &analysis, cursor);
    match analysis.context {
        ContextKind::None => {}
        ContextKind::StatementStart => run.statement_start(),
        ContextKind::Relation => run.relation(),
        ContextKind::RelationTail => run.relation_tail(),
        ContextKind::Expression => run.expression(),
        ContextKind::Qualified => run.qualified(),
        ContextKind::InsertColumns => run.insert_columns(),
    }
    let (items, requests) = run.finish();
    Completions {
        items,
        replace: analysis.prefix.clone(),
        context: analysis.context,
        requests,
    }
}

/// How a candidate is turned into an item.
enum Extra<'a> {
    Relation {
        schema: Option<&'a str>,
        kind: ObjectKind,
    },
    Schema,
    Cte,
    Alias {
        target: &'a str,
    },
    Column {
        info: Option<&'a ColumnInfo>,
        qualifier: Cow<'a, str>,
        qualify: bool,
    },
    Func(&'static Func),
    Keyword,
    Snippet(&'static Snippet),
}

struct Cand<'a> {
    label: Cow<'a, str>,
    kind: ItemKind,
    group: u8,
    class: Class,
    gap: u32,
    seq: u32,
    extra: Extra<'a>,
}

fn cmp_cands(a: &Cand<'_>, b: &Cand<'_>) -> std::cmp::Ordering {
    a.class
        .cmp(&b.class)
        .then(a.group.cmp(&b.group))
        .then(a.gap.cmp(&b.gap))
        .then(a.label.len().cmp(&b.label.len()))
        .then_with(|| cmp_labels(&a.label, &b.label))
        .then(a.seq.cmp(&b.seq))
}

/// The columns of a relation as the engine sees them.
#[derive(Clone, Copy)]
enum Cols<'a> {
    Info(&'a [ColumnInfo]),
    Names(&'a [String]),
}

struct Run<'a> {
    req: &'a CompletionRequest<'a>,
    schema: &'a dyn SchemaSource,
    analysis: &'a Analysis,
    dialect: Dialect,
    matcher: Matcher,
    cands: Vec<Cand<'a>>,
    requests: Vec<SchemaRequest>,
    lower_keywords: bool,
    force_quote: bool,
}

impl<'a> Run<'a> {
    fn new(
        req: &'a CompletionRequest<'a>,
        schema: &'a dyn SchemaSource,
        analysis: &'a Analysis,
        cursor: usize,
    ) -> Self {
        let end = analysis.prefix.end.min(cursor);
        let raw = req.text.get(analysis.prefix.start..end).unwrap_or("");
        let force_quote = raw.starts_with(['"', '`', '[']);
        let typed = if force_quote { &raw[1..] } else { raw };
        Self {
            req,
            schema,
            analysis,
            dialect: req.dialect,
            matcher: Matcher::new(typed),
            cands: Vec::new(),
            requests: Vec::new(),
            lower_keywords: !typed.is_empty() && !typed.chars().any(char::is_uppercase),
            force_quote,
        }
    }

    fn push(
        &mut self,
        label: impl Into<Cow<'a, str>>,
        kind: ItemKind,
        group: u8,
        extra: Extra<'a>,
    ) {
        let label = label.into();
        if let Some((class, gap)) = self.matcher.test(&label) {
            // Fuzzy matching is for names from the schema; fixed vocabularies match by prefix.
            let fixed = matches!(
                kind,
                ItemKind::Keyword | ItemKind::Function | ItemKind::Snippet
            );
            if fixed && class >= FUZZY {
                return;
            }
            let seq = u32::try_from(self.cands.len()).unwrap_or(u32::MAX);
            self.cands.push(Cand {
                label,
                kind,
                group,
                class,
                gap,
                seq,
                extra,
            });
        }
    }

    fn request(&mut self, r: SchemaRequest) {
        if !self.requests.contains(&r) {
            self.requests.push(r);
        }
    }

    fn keyword_list(&mut self, list: impl Iterator<Item = &'static str>, group: u8) {
        for kw in list {
            self.push(kw, ItemKind::Keyword, group, Extra::Keyword);
        }
    }

    /// Pushes keywords that keep the order of the list (most useful first).
    fn ordered_keywords(&mut self, list: &'static [&'static str]) {
        for (i, kw) in list.iter().enumerate() {
            let group = u8::try_from(i).unwrap_or(u8::MAX - 1).min(99);
            self.push(*kw, ItemKind::Keyword, group, Extra::Keyword);
        }
    }

    fn snippets(&mut self, group: u8) {
        for s in SNIPPETS {
            self.push(s.trigger, ItemKind::Snippet, group, Extra::Snippet(s));
        }
    }

    fn statement_start(&mut self) {
        self.ordered_keywords(statement_keywords(self.dialect));
        self.snippets(100);
    }

    fn relation_tail(&mut self) {
        self.ordered_keywords(TAIL_KEYWORDS);
    }

    /// Pushes the tables, views and materialized views of `rels`.
    fn push_relations(
        &mut self,
        rels: impl Iterator<Item = crate::source::Relation<'a>>,
        group: u8,
    ) {
        for r in rels {
            let kind = match r.kind {
                ObjectKind::Table => ItemKind::Table,
                ObjectKind::View => ItemKind::View,
                ObjectKind::MaterializedView => ItemKind::MaterializedView,
                _ => continue,
            };
            self.push(
                r.name,
                kind,
                group,
                Extra::Relation {
                    schema: r.schema,
                    kind: r.kind,
                },
            );
        }
    }

    fn relation(&mut self) {
        let schema = self.schema;
        let analysis = self.analysis;
        for cte in &analysis.scope.ctes {
            self.push(
                text_name(self.dialect, &cte.name),
                ItemKind::Cte,
                0,
                Extra::Cte,
            );
        }
        match schema.relations(None) {
            Loadable::Loaded(rels) => self.push_relations(rels, 1),
            Loadable::NotLoaded => self.request(SchemaRequest::Relations { schema: None }),
        }
        for s in schema.schemas() {
            self.push(s, ItemKind::Schema, 2, Extra::Schema);
        }
        self.keyword_list(RELATION_KEYWORDS.iter().copied(), 3);
    }

    fn columns_of(&mut self, schema: Option<&'a str>, relation: &'a str) -> Option<Cols<'a>> {
        let src = self.schema;
        match src.columns(schema, relation) {
            Loadable::Loaded(c) => Some(Cols::Info(c)),
            Loadable::NotLoaded => {
                self.request(SchemaRequest::Columns {
                    schema: schema.map(str::to_owned),
                    relation: relation.to_owned(),
                });
                None
            }
        }
    }

    /// Resolves a typed (optionally schema-qualified) table name to its columns.
    fn table_cols(&mut self, schema: Option<&'a Ident>, name: &'a Ident) -> Option<Cols<'a>> {
        let src = self.schema;
        let dialect = self.dialect;
        let stored_schema: Option<&'a str> =
            schema.map(|s| find_stored(dialect, s, src.schemas()).unwrap_or(s.value.as_str()));
        match src.relations(stored_schema) {
            Loadable::Loaded(rels) => {
                let rel = find_stored_by(dialect, name, rels, |r| r.name)?;
                self.columns_of(rel.schema.or(stored_schema), rel.name)
            }
            Loadable::NotLoaded => {
                self.request(SchemaRequest::Relations {
                    schema: stored_schema.map(str::to_owned),
                });
                self.columns_of(stored_schema, name.value.as_str())
            }
        }
    }

    fn relation_cols(&mut self, rel: &'a ScopeRelation) -> Option<Cols<'a>> {
        match &rel.source {
            RelationSource::Table { schema, name } => self.table_cols(schema.as_ref(), name),
            RelationSource::Cte { name } => self
                .analysis
                .scope
                .ctes
                .iter()
                .find(|c| idents_match(self.dialect, &c.name, name))
                .map(|c| Cols::Names(&c.columns)),
            RelationSource::Subquery { columns } => Some(Cols::Names(columns)),
        }
    }

    // The `Cow` is cloned into every column candidate, so it is taken by reference.
    #[allow(clippy::ptr_arg)]
    fn push_columns(&mut self, cols: Cols<'a>, qualifier: &Cow<'a, str>, group: u8) {
        match cols {
            Cols::Info(infos) => {
                for info in infos {
                    self.push(
                        &info.name,
                        ItemKind::Column,
                        group,
                        Extra::Column {
                            info: Some(info),
                            qualifier: qualifier.clone(),
                            qualify: false,
                        },
                    );
                }
            }
            Cols::Names(names) => {
                for name in names {
                    self.push(
                        name,
                        ItemKind::Column,
                        group,
                        Extra::Column {
                            info: None,
                            qualifier: qualifier.clone(),
                            qualify: false,
                        },
                    );
                }
            }
        }
    }

    fn insert_columns(&mut self) {
        let analysis = self.analysis;
        if let Some(rel) = analysis.scope.relations.first()
            && let Some(cols) = self.relation_cols(rel)
        {
            self.push_columns(cols, &Cow::Borrowed(""), 0);
        }
    }

    fn expression(&mut self) {
        let analysis = self.analysis;
        let first_col = self.cands.len();
        for (idx, rel) in analysis.scope.relations.iter().enumerate() {
            let visible = rel
                .visible_name()
                .map_or(Cow::Borrowed(""), |i| text_name(self.dialect, i));
            let group = u8::from(idx >= analysis.scope.local);
            if let Some(cols) = self.relation_cols(rel) {
                self.push_columns(cols, &visible, group);
            }
            if visible.is_empty() {
                continue;
            }
            let (kind, target) = match (&rel.alias, &rel.source) {
                (None, RelationSource::Cte { .. }) => (ItemKind::Cte, "cte"),
                (None, _) => (ItemKind::Alias, "table"),
                (Some(_), RelationSource::Table { name, .. } | RelationSource::Cte { name }) => {
                    (ItemKind::Alias, name.value.as_str())
                }
                (Some(_), RelationSource::Subquery { .. }) => (ItemKind::Alias, "subquery"),
            };
            self.push(visible, kind, 2, Extra::Alias { target });
        }
        self.mark_ambiguous(first_col);
        for func in functions(self.dialect) {
            self.push(func.name, ItemKind::Function, 3, Extra::Func(func));
        }
        self.keyword_list(keywords(self.dialect), 4);
        if !self.matcher.is_empty() {
            self.snippets(5);
        }
    }

    /// Columns whose name occurs in more than one relation are inserted qualified.
    fn mark_ambiguous(&mut self, from: usize) {
        let mut counts: HashMap<String, u32> = HashMap::new();
        for c in &self.cands[from..] {
            if matches!(c.extra, Extra::Column { .. }) {
                *counts
                    .entry(c.label.chars().map(fold).collect())
                    .or_default() += 1;
            }
        }
        for c in &mut self.cands[from..] {
            if let Extra::Column { qualify, .. } = &mut c.extra {
                let key: String = c.label.chars().map(fold).collect();
                *qualify = counts.get(&key).is_some_and(|&n| n > 1);
            }
        }
    }

    fn qualified(&mut self) {
        let analysis = self.analysis;
        let dialect = self.dialect;
        let q = &analysis.qualifier;
        let Some(last) = q.last() else { return };
        if q.len() >= 2 {
            let schema = &q[q.len() - 2];
            if let Some(cols) = self.table_cols(Some(schema), last) {
                self.push_columns(cols, &text_name(dialect, last), 0);
            }
            return;
        }
        if let Some(rel) = analysis.scope.resolve(last, dialect) {
            if let Some(cols) = self.relation_cols(rel) {
                self.push_columns(cols, &text_name(dialect, last), 0);
            }
            return;
        }
        let src = self.schema;
        if let Some(stored) = find_stored(dialect, last, src.schemas()) {
            match src.relations(Some(stored)) {
                Loadable::Loaded(rels) => self.push_relations(rels, 0),
                Loadable::NotLoaded => self.request(SchemaRequest::Relations {
                    schema: Some(stored.to_owned()),
                }),
            }
        } else if let Some(cols) = self.table_cols(None, last) {
            self.push_columns(cols, &text_name(dialect, last), 0);
        }
    }

    fn finish(mut self) -> (Vec<CompletionItem>, Vec<SchemaRequest>) {
        let limit = self.req.limit;
        let mut cands = std::mem::take(&mut self.cands);
        if cands.len() > limit && limit > 0 {
            cands.select_nth_unstable_by(limit - 1, cmp_cands);
            cands.truncate(limit);
        }
        cands.truncate(limit);
        cands.sort_unstable_by(cmp_cands);
        // A candidate whose name cannot be quoted is dropped, never inserted raw.
        let mut items: Vec<CompletionItem> = cands.iter().filter_map(|c| self.build(c)).collect();
        let n = items.len();
        for (i, item) in items.iter_mut().enumerate() {
            item.score = u32::try_from(n - i).unwrap_or(u32::MAX);
        }
        (items, self.requests)
    }

    fn quote(&self, name: &str) -> Option<String> {
        quote(self.dialect, name, self.force_quote)
    }

    fn build(&self, c: &Cand<'_>) -> Option<CompletionItem> {
        let label: &str = &c.label;
        let fixed = matches!(c.extra, Extra::Func(_) | Extra::Keyword | Extra::Snippet(_));
        let mut item = CompletionItem {
            label: label.to_owned(),
            kind: c.kind,
            insert_text: if fixed {
                String::new()
            } else {
                self.quote(label)?
            },
            format: InsertFormat::Plain,
            detail: None,
            score: 0,
        };
        match &c.extra {
            Extra::Relation { schema, kind } => {
                item.detail = Some(match schema {
                    Some(s) => format!("{s} \u{b7} {}", kind.as_str()),
                    None => kind.as_str().to_owned(),
                });
            }
            Extra::Schema => item.detail = Some("schema".to_owned()),
            Extra::Cte => item.detail = Some("cte".to_owned()),
            Extra::Alias { target } => item.detail = Some((*target).to_owned()),
            Extra::Column {
                info,
                qualifier,
                qualify,
            } => {
                if *qualify && !qualifier.is_empty() {
                    item.insert_text = format!(
                        "{}.{}",
                        quote(self.dialect, qualifier, false)?,
                        self.quote(label)?
                    );
                }
                let ty = info.map(|i| {
                    i.native_type
                        .clone()
                        .unwrap_or_else(|| format!("{:?}", i.data_type))
                });
                item.detail = match (qualifier.is_empty(), ty) {
                    (false, Some(ty)) => Some(format!("{qualifier} \u{b7} {ty}")),
                    (false, None) => Some(qualifier.to_string()),
                    (true, Some(ty)) => Some(ty),
                    (true, None) => None,
                };
            }
            Extra::Func(f) => {
                if f.nullary {
                    item.insert_text = format!("{}()", f.name);
                } else {
                    item.insert_text = format!("{}($1)", f.name);
                    item.format = InsertFormat::Snippet;
                }
                item.detail = Some(f.sig.to_owned());
            }
            Extra::Keyword => {
                item.label = apply_case(label, self.lower_keywords);
                item.insert_text.clone_from(&item.label);
            }
            Extra::Snippet(s) => {
                item.insert_text = apply_case(s.body, self.lower_keywords);
                item.format = InsertFormat::Snippet;
                item.detail = Some(s.detail.to_owned());
            }
        }
        Some(item)
    }
}
