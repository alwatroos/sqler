// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Scope analysis: one pass over the tokens of a statement that tracks clauses per parenthesis
//! level and collects CTEs, table references, aliases and subquery aliases.

use std::ops::Range;

use sqler_sql::Dialect;

use crate::dialect_data::{idents_match, text_name};
use crate::lexer::{TokKind, Token};

/// An identifier as written in the SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ident {
    /// The name without quotes; doubled quote characters are un-doubled.
    pub value: String,
    /// Whether it was quoted.
    pub quoted: bool,
    /// Byte span in the text, quotes included.
    pub span: Range<usize>,
}

impl Ident {
    /// Builds an identifier (span `0..0`); mainly for tests and callers that look names up.
    #[must_use]
    pub fn new(value: impl Into<String>, quoted: bool) -> Self {
        Self {
            value: value.into(),
            quoted,
            span: 0..0,
        }
    }
}

/// A common table expression.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Cte {
    /// The CTE name.
    pub name: Ident,
    /// Its columns: the declared list, else the names the body projects.
    pub columns: Vec<String>,
    /// Byte span from the name to the closing parenthesis.
    pub span: Range<usize>,
}

/// What a relation in a `FROM` clause refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelationSource {
    /// A table or view, optionally schema-qualified.
    Table {
        /// The schema, if written.
        schema: Option<Ident>,
        /// The table or view name.
        name: Ident,
    },
    /// A reference to a CTE.
    Cte {
        /// The CTE name.
        name: Ident,
    },
    /// A subquery (or table function) with the column names it exposes.
    Subquery {
        /// Projected column names; `*` contributes nothing.
        columns: Vec<String>,
    },
}

/// One relation in scope.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScopeRelation {
    /// The alias, if any.
    pub alias: Option<Ident>,
    /// What it refers to.
    pub source: RelationSource,
    /// Byte span of the whole reference, alias included.
    pub span: Range<usize>,
}

impl ScopeRelation {
    /// The name this relation is visible as: the alias, else the table or CTE name.
    #[must_use]
    pub fn visible_name(&self) -> Option<&Ident> {
        self.alias.as_ref().or(match &self.source {
            RelationSource::Table { name, .. } | RelationSource::Cte { name } => Some(name),
            RelationSource::Subquery { .. } => None,
        })
    }
}

/// The names visible at the cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Scope {
    /// CTEs defined by the enclosing `WITH` clauses.
    pub ctes: Vec<Cte>,
    /// Relations of the cursor's query level first, then those of the enclosing levels.
    pub relations: Vec<ScopeRelation>,
    /// How many of the leading `relations` belong to the cursor's own query level.
    pub local: usize,
}

impl Scope {
    /// Resolves a qualifier to a relation: by alias first, else by bare table or CTE name.
    #[must_use]
    pub fn resolve(&self, qualifier: &Ident, dialect: Dialect) -> Option<&ScopeRelation> {
        let by_alias = self.relations.iter().find(|r| {
            r.alias
                .as_ref()
                .is_some_and(|a| idents_match(dialect, qualifier, a))
        });
        by_alias.or_else(|| {
            self.relations.iter().find(|r| match &r.source {
                RelationSource::Table { name, .. } | RelationSource::Cte { name } => {
                    idents_match(dialect, qualifier, name)
                }
                RelationSource::Subquery { .. } => false,
            })
        })
    }
}

/// Builds an [`Ident`] from a `Word` or `Quoted` token.
pub(crate) fn ident_from_token(text: &str, t: &Token) -> Ident {
    let raw = t.text(text);
    if t.kind != TokKind::Quoted {
        return Ident {
            value: raw.to_owned(),
            quoted: false,
            span: t.start..t.end,
        };
    }
    let open = raw.chars().next().unwrap_or('"');
    let close = if open == '[' { ']' } else { open };
    let inner = raw.get(open.len_utf8()..).unwrap_or("");
    let inner = if t.unterminated {
        inner
    } else {
        inner.strip_suffix(close).unwrap_or(inner)
    };
    let value = if open == '[' {
        inner.to_owned()
    } else {
        let doubled: String = [close, close].iter().collect();
        inner.replace(&doubled, &close.to_string())
    };
    Ident {
        value,
        quoted: true,
        span: t.start..t.end,
    }
}

/// The clause a parenthesis level is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clause {
    /// Nothing yet: a statement (or subquery) can start here.
    Start,
    With,
    Select,
    From,
    Update,
    Into,
    Delete,
    Insert,
    Where,
    On,
    Group,
    Order,
    Having,
    Set,
    Returning,
    Values,
    /// Inside a function call or other expression parentheses.
    Expr,
    Other,
}

/// Where a relation reference stands: expecting a name, after a name, after `AS`, done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fs {
    Expect,
    Name,
    NameDot,
    AsKw,
    Done,
}

/// What opened a parenthesis level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LevelKind {
    Root,
    Sub,
    Expr,
    InsertCols,
    ValuesRow,
    CteBody,
    CteCols,
}

/// The state of the innermost level at the cursor.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Snapshot {
    pub clause: Clause,
    pub fs: Fs,
    pub kind: LevelKind,
    pub level: usize,
}

#[derive(Debug, Default)]
struct Level {
    parent: Option<usize>,
    relations: Vec<ScopeRelation>,
    ctes: Vec<Cte>,
}

/// The result of scanning a statement.
#[derive(Debug)]
pub(crate) struct Scan {
    levels: Vec<Level>,
    pub top: Snapshot,
}

impl Scan {
    /// The scope visible from the snapshot's level: its own relations, then outer ones.
    pub(crate) fn scope(&self) -> Scope {
        let mut scope = Scope::default();
        let mut next = Some(self.top.level);
        let mut first = true;
        while let Some(l) = next {
            let level = &self.levels[l];
            if first {
                scope.local = level.relations.len();
                first = false;
            }
            scope.relations.extend(level.relations.iter().cloned());
            scope.ctes.extend(level.ctes.iter().cloned());
            next = level.parent;
        }
        scope
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kw {
    Select,
    From,
    Where,
    Group,
    Order,
    By,
    Having,
    Limit,
    Offset,
    Fetch,
    For,
    Window,
    Union,
    Intersect,
    Except,
    Join,
    Inner,
    Left,
    Right,
    Full,
    Cross,
    Natural,
    Outer,
    Lateral,
    Only,
    If,
    Exists,
    Not,
    On,
    Using,
    Set,
    Values,
    Returning,
    As,
    With,
    Recursive,
    Insert,
    Replace,
    Into,
    Update,
    Delete,
    Table,
    Distinct,
    All,
    Is,
    Materialized,
    Other,
}

fn kw_of(word: &str) -> Kw {
    let mut buf = [0u8; 12];
    let bytes = word.as_bytes();
    if bytes.len() > buf.len() || !word.is_ascii() {
        return Kw::Other;
    }
    for (dst, src) in buf.iter_mut().zip(bytes) {
        *dst = src.to_ascii_lowercase();
    }
    match &buf[..bytes.len()] {
        b"select" => Kw::Select,
        b"from" => Kw::From,
        b"where" => Kw::Where,
        b"group" => Kw::Group,
        b"order" => Kw::Order,
        b"by" => Kw::By,
        b"having" => Kw::Having,
        b"limit" => Kw::Limit,
        b"offset" => Kw::Offset,
        b"fetch" => Kw::Fetch,
        b"for" => Kw::For,
        b"window" => Kw::Window,
        b"union" => Kw::Union,
        b"intersect" => Kw::Intersect,
        b"except" => Kw::Except,
        b"join" => Kw::Join,
        b"inner" => Kw::Inner,
        b"left" => Kw::Left,
        b"right" => Kw::Right,
        b"full" => Kw::Full,
        b"cross" => Kw::Cross,
        b"natural" => Kw::Natural,
        b"outer" => Kw::Outer,
        b"lateral" => Kw::Lateral,
        b"only" => Kw::Only,
        b"if" => Kw::If,
        b"exists" => Kw::Exists,
        b"not" => Kw::Not,
        b"on" => Kw::On,
        b"using" => Kw::Using,
        b"set" => Kw::Set,
        b"values" => Kw::Values,
        b"returning" => Kw::Returning,
        b"as" => Kw::As,
        b"with" => Kw::With,
        b"recursive" => Kw::Recursive,
        b"insert" => Kw::Insert,
        b"replace" => Kw::Replace,
        b"into" => Kw::Into,
        b"update" => Kw::Update,
        b"delete" => Kw::Delete,
        b"table" => Kw::Table,
        b"distinct" => Kw::Distinct,
        b"all" => Kw::All,
        b"is" => Kw::Is,
        b"materialized" => Kw::Materialized,
        _ => Kw::Other,
    }
}

#[derive(Debug)]
enum Pending {
    Parts(Vec<Ident>),
    Sub {
        columns: Vec<String>,
        span: Range<usize>,
    },
    Func(Range<usize>),
}

#[derive(Debug)]
struct Frame {
    level: usize,
    clause: Clause,
    fs: Fs,
    kind: LevelKind,
    open_idx: usize,
    open_start: usize,
    pending: Option<Pending>,
    /// Token range of the select list: start, and end once a later clause starts.
    sel: Option<(usize, Option<usize>)>,
    cte_name: Option<Ident>,
    cte_cols: Option<Vec<String>>,
    cte_as: bool,
}

impl Frame {
    fn new(level: usize, kind: LevelKind, open_idx: usize, open_start: usize) -> Self {
        let clause = match kind {
            LevelKind::Root | LevelKind::Sub | LevelKind::CteBody => Clause::Start,
            LevelKind::Expr => Clause::Expr,
            LevelKind::InsertCols | LevelKind::CteCols => Clause::Other,
            LevelKind::ValuesRow => Clause::Values,
        };
        Self {
            level,
            clause,
            fs: Fs::Expect,
            kind,
            open_idx,
            open_start,
            pending: None,
            sel: None,
            cte_name: None,
            cte_cols: None,
            cte_as: false,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            clause: self.clause,
            fs: self.fs,
            kind: self.kind,
            level: self.level,
        }
    }

    fn is_relation_clause(&self) -> bool {
        matches!(self.clause, Clause::From | Clause::Update | Clause::Into)
    }

    fn close_select_list(&mut self, idx: usize) {
        if let Some((_, end @ None)) = &mut self.sel {
            *end = Some(idx);
        }
    }
}

struct Scanner<'a> {
    text: &'a str,
    toks: &'a [Token],
    levels: Vec<Level>,
    stack: Vec<Frame>,
    snapshot: Option<Snapshot>,
    dialect: Dialect,
}

/// Scans the tokens of one statement (comments removed).
///
/// `prefix_start` is where the identifier being typed starts (or the cursor); `skip_prefix`
/// drops that token from the scan, so a half-typed name does not become a relation. Output column
/// names of CTEs and subqueries are stored the way the dialect resolves them (`text_name`).
pub(crate) fn scan(
    text: &str,
    toks: &[Token],
    prefix_start: usize,
    skip_prefix: bool,
    dialect: Dialect,
) -> Scan {
    let mut s = Scanner {
        dialect,
        text,
        toks,
        levels: vec![Level::default()],
        stack: vec![Frame::new(0, LevelKind::Root, 0, 0)],
        snapshot: None,
    };
    for (i, t) in toks.iter().enumerate() {
        if s.snapshot.is_none() && t.start >= prefix_start {
            s.snapshot = s.stack.last().map(Frame::snapshot);
        }
        if skip_prefix
            && t.start == prefix_start
            && matches!(t.kind, TokKind::Word | TokKind::Quoted)
        {
            continue;
        }
        s.step(i, t);
    }
    let top = s
        .snapshot
        .or_else(|| s.stack.last().map(Frame::snapshot))
        .unwrap_or(Snapshot {
            clause: Clause::Start,
            fs: Fs::Expect,
            kind: LevelKind::Root,
            level: 0,
        });
    while let Some(mut f) = s.stack.pop() {
        s.flush(&mut f);
    }
    Scan {
        levels: s.levels,
        top,
    }
}

impl Scanner<'_> {
    fn word_kw(&self, idx: usize) -> Option<Kw> {
        let t = self.toks.get(idx)?;
        (t.kind == TokKind::Word).then(|| kw_of(t.text(self.text)))
    }

    fn step(&mut self, i: usize, t: &Token) {
        match t.kind {
            TokKind::Word => {
                let k = kw_of(t.text(self.text));
                if k == Kw::Other {
                    self.ident(ident_from_token(self.text, t));
                } else {
                    self.keyword(i, k);
                }
            }
            TokKind::Quoted => self.ident(ident_from_token(self.text, t)),
            TokKind::Punct => match t.text(self.text) {
                "(" => self.open(i, t),
                ")" => self.close(i, t),
                "," => self.comma(),
                "." => self.dot(),
                _ => {}
            },
            TokKind::Str | TokKind::Number | TokKind::Comment => {}
        }
    }

    /// Registers a pending relation (without alias) when leaving the relation state.
    fn flush(&mut self, f: &mut Frame) {
        if matches!(f.fs, Fs::Name | Fs::AsKw) {
            if let Some(p) = f.pending.take() {
                self.finish_relation(f.level, p, None);
            }
        } else {
            f.pending = None;
        }
    }

    fn finish_relation(&mut self, level: usize, pending: Pending, alias: Option<Ident>) {
        let alias_end = alias.as_ref().map(|a| a.span.end);
        let (source, span) = match pending {
            Pending::Parts(mut parts) => {
                let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
                    return;
                };
                let span = first.span.start..alias_end.unwrap_or(last.span.end);
                let Some(name) = parts.pop() else { return };
                let schema = parts.pop();
                let source = if schema.is_none() && self.cte_visible(level, &name) {
                    RelationSource::Cte { name }
                } else {
                    RelationSource::Table { schema, name }
                };
                (source, span)
            }
            Pending::Sub { columns, span } => (
                RelationSource::Subquery { columns },
                span.start..alias_end.unwrap_or(span.end),
            ),
            Pending::Func(span) => {
                if alias.is_none() {
                    return;
                }
                (
                    RelationSource::Subquery {
                        columns: Vec::new(),
                    },
                    span.start..alias_end.unwrap_or(span.end),
                )
            }
        };
        self.levels[level].relations.push(ScopeRelation {
            alias,
            source,
            span,
        });
    }

    fn cte_visible(&self, level: usize, name: &Ident) -> bool {
        let mut next = Some(level);
        while let Some(l) = next {
            let lv = &self.levels[l];
            if lv
                .ctes
                .iter()
                .any(|c| idents_match(self.dialect, &c.name, name))
            {
                return true;
            }
            next = lv.parent;
        }
        false
    }

    fn ident(&mut self, id: Ident) {
        let Some(mut f) = self.stack.pop() else {
            return;
        };
        if f.is_relation_clause() {
            match f.fs {
                Fs::Expect => {
                    f.pending = Some(Pending::Parts(vec![id]));
                    f.fs = Fs::Name;
                }
                Fs::NameDot => {
                    if let Some(Pending::Parts(parts)) = &mut f.pending {
                        parts.push(id);
                    }
                    f.fs = Fs::Name;
                }
                Fs::Name | Fs::AsKw => {
                    if let Some(p) = f.pending.take() {
                        self.finish_relation(f.level, p, Some(id));
                    }
                    f.fs = Fs::Done;
                }
                Fs::Done => {}
            }
        } else if f.clause == Clause::With && f.cte_name.is_none() && !f.cte_as {
            f.cte_name = Some(id);
        }
        self.stack.push(f);
    }

    fn comma(&mut self) {
        let Some(mut f) = self.stack.pop() else {
            return;
        };
        match f.clause {
            Clause::From | Clause::Update | Clause::Into => {
                self.flush(&mut f);
                f.fs = Fs::Expect;
            }
            Clause::On => {
                self.flush(&mut f);
                f.clause = Clause::From;
                f.fs = Fs::Expect;
            }
            _ => {}
        }
        self.stack.push(f);
    }

    fn dot(&mut self) {
        if let Some(f) = self.stack.last_mut()
            && f.is_relation_clause()
            && f.fs == Fs::Name
            && matches!(f.pending, Some(Pending::Parts(_)))
        {
            f.fs = Fs::NameDot;
        }
    }

    fn open(&mut self, i: usize, t: &Token) {
        let next_select = matches!(
            self.word_kw(i + 1),
            Some(Kw::Select | Kw::With | Kw::Values)
        );
        let Some(mut f) = self.stack.pop() else {
            return;
        };
        let kind = match (f.clause, f.fs) {
            (Clause::From | Clause::Update, Fs::Expect) => LevelKind::Sub,
            (Clause::Into, Fs::Name | Fs::Done) => {
                self.flush(&mut f);
                LevelKind::InsertCols
            }
            (Clause::From | Clause::Update | Clause::Into, _) => LevelKind::Expr,
            (Clause::With, _) => {
                if f.cte_as {
                    LevelKind::CteBody
                } else if f.cte_name.is_some() {
                    LevelKind::CteCols
                } else {
                    LevelKind::Expr
                }
            }
            (Clause::Values, _) => LevelKind::ValuesRow,
            _ if next_select => LevelKind::Sub,
            _ => LevelKind::Expr,
        };
        let parent = f.level;
        self.stack.push(f);
        let level = self.levels.len();
        self.levels.push(Level {
            parent: Some(parent),
            ..Level::default()
        });
        self.stack.push(Frame::new(level, kind, i, t.start));
    }

    fn close(&mut self, i: usize, t: &Token) {
        if self.stack.len() <= 1 {
            return;
        }
        let Some(mut child) = self.stack.pop() else {
            return;
        };
        self.flush(&mut child);
        child.close_select_list(i);
        let Some(mut parent) = self.stack.pop() else {
            return;
        };
        match child.kind {
            LevelKind::Sub if parent.is_relation_clause() && parent.fs == Fs::Expect => {
                let columns = self.projection(&child);
                parent.pending = Some(Pending::Sub {
                    columns,
                    span: child.open_start..t.end,
                });
                parent.fs = Fs::Name;
            }
            LevelKind::Expr
                if parent.is_relation_clause()
                    && parent.fs == Fs::Name
                    && matches!(parent.pending, Some(Pending::Parts(_))) =>
            {
                let start = match &parent.pending {
                    Some(Pending::Parts(p)) => p.first().map_or(child.open_start, |x| x.span.start),
                    _ => child.open_start,
                };
                parent.pending = Some(Pending::Func(start..t.end));
            }
            LevelKind::CteBody => {
                if let Some(name) = parent.cte_name.take() {
                    let columns = parent
                        .cte_cols
                        .take()
                        .unwrap_or_else(|| self.projection(&child));
                    let span = name.span.start..t.end;
                    self.levels[parent.level].ctes.push(Cte {
                        name,
                        columns,
                        span,
                    });
                }
                parent.cte_as = false;
            }
            LevelKind::CteCols => {
                let cols = self
                    .toks
                    .get(child.open_idx + 1..i)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|x| matches!(x.kind, TokKind::Word | TokKind::Quoted))
                    .map(|x| self.column_name(x))
                    .collect();
                parent.cte_cols = Some(cols);
            }
            _ => {}
        }
        self.stack.push(parent);
    }

    #[allow(clippy::too_many_lines)]
    fn keyword(&mut self, i: usize, k: Kw) {
        let next_paren = self
            .toks
            .get(i + 1)
            .is_some_and(|t| t.is_punct(self.text, '('));
        let next_conflict = self.toks.get(i + 1).is_some_and(|t| {
            let w = t.text(self.text);
            w.eq_ignore_ascii_case("conflict") || w.eq_ignore_ascii_case("duplicate")
        });
        let next_into = self.word_kw(i + 1) == Some(Kw::Into);
        let after_dot = i > 0
            && self.toks[i - 1].is_punct(self.text, '.')
            && self.toks[i - 1].end == self.toks[i].start;
        let is_distinct_from = i >= 2
            && self.word_kw(i - 1) == Some(Kw::Distinct)
            && self.word_kw(i - 2) == Some(Kw::Is);
        let Some(mut f) = self.stack.pop() else {
            return;
        };
        if after_dot
            || !matches!(
                f.kind,
                LevelKind::Root | LevelKind::Sub | LevelKind::CteBody
            )
        {
            self.stack.push(f);
            return;
        }
        if matches!(
            k,
            Kw::From
                | Kw::Where
                | Kw::Group
                | Kw::Order
                | Kw::Having
                | Kw::Limit
                | Kw::Offset
                | Kw::Fetch
                | Kw::Window
                | Kw::Union
                | Kw::Intersect
                | Kw::Except
        ) && f.clause == Clause::Select
            && !is_distinct_from
        {
            f.close_select_list(i);
        }
        let at_start = matches!(f.clause, Clause::Start | Clause::With);
        let rel = f.is_relation_clause();
        match k {
            Kw::Select => {
                self.flush(&mut f);
                f.clause = Clause::Select;
                f.sel = Some((i + 1, None));
            }
            Kw::From if is_distinct_from => {}
            Kw::From | Kw::Join => {
                self.flush(&mut f);
                f.clause = Clause::From;
                f.fs = Fs::Expect;
            }
            Kw::Inner | Kw::Left | Kw::Right | Kw::Full | Kw::Cross | Kw::Natural | Kw::Outer
                if (rel || f.clause == Clause::On) && !next_paren =>
            {
                self.flush(&mut f);
                f.clause = Clause::From;
                f.fs = Fs::Done;
            }
            Kw::On if next_conflict => {
                self.flush(&mut f);
                f.clause = Clause::Other;
            }
            Kw::On | Kw::Using if rel => {
                self.flush(&mut f);
                f.clause = Clause::On;
            }
            Kw::Where => self.switch(&mut f, Clause::Where),
            Kw::Group => self.switch(&mut f, Clause::Group),
            Kw::Order => self.switch(&mut f, Clause::Order),
            Kw::Having => self.switch(&mut f, Clause::Having),
            Kw::Limit | Kw::Offset | Kw::Fetch | Kw::For | Kw::Window => {
                self.switch(&mut f, Clause::Other);
            }
            Kw::Union | Kw::Intersect | Kw::Except => {
                self.switch(&mut f, Clause::Start);
                f.sel = None;
            }
            Kw::Set => self.switch(&mut f, Clause::Set),
            Kw::Values => self.switch(&mut f, Clause::Values),
            Kw::Returning => self.switch(&mut f, Clause::Returning),
            Kw::With if f.clause == Clause::Start => {
                f.clause = Clause::With;
                f.cte_name = None;
                f.cte_cols = None;
                f.cte_as = false;
            }
            Kw::Insert if at_start => f.clause = Clause::Insert,
            Kw::Replace if at_start && next_into => f.clause = Clause::Insert,
            Kw::Into if f.clause == Clause::Insert => {
                f.clause = Clause::Into;
                f.fs = Fs::Expect;
            }
            Kw::Update if at_start => {
                f.clause = Clause::Update;
                f.fs = Fs::Expect;
            }
            Kw::Delete if at_start => f.clause = Clause::Delete,
            Kw::Table if f.clause == Clause::Start => {
                f.clause = Clause::From;
                f.fs = Fs::Expect;
            }
            Kw::As => {
                if rel && f.fs == Fs::Name {
                    f.fs = Fs::AsKw;
                } else if f.clause == Clause::With && f.cte_name.is_some() {
                    f.cte_as = true;
                }
            }
            _ => {}
        }
        self.stack.push(f);
    }

    fn switch(&mut self, f: &mut Frame, clause: Clause) {
        self.flush(f);
        f.clause = clause;
    }

    /// The column names a select list projects.
    fn projection(&self, f: &Frame) -> Vec<String> {
        let Some((start, end)) = f.sel else {
            return Vec::new();
        };
        let end = end.unwrap_or(self.toks.len());
        let Some(toks) = self.toks.get(start..end) else {
            return Vec::new();
        };
        let mut items: Vec<&[Token]> = Vec::new();
        let mut depth = 0i32;
        let mut from = 0;
        for (k, t) in toks.iter().enumerate() {
            if t.is_punct(self.text, '(') {
                depth += 1;
            } else if t.is_punct(self.text, ')') {
                depth -= 1;
            } else if depth == 0 && t.is_punct(self.text, ',') {
                items.push(&toks[from..k]);
                from = k + 1;
            }
        }
        items.push(&toks[from..]);
        if let Some(first) = items.first_mut() {
            while first
                .first()
                .is_some_and(|t| matches!(self.kw(t), Some(Kw::Distinct | Kw::All)))
            {
                *first = &first[1..];
            }
        }
        items
            .into_iter()
            .filter_map(|it| self.item_name(it))
            .collect()
    }

    /// The name a `Word` or `Quoted` token gives a column, folded the way the dialect does.
    fn column_name(&self, t: &Token) -> String {
        text_name(self.dialect, &ident_from_token(self.text, t)).into_owned()
    }

    fn kw(&self, t: &Token) -> Option<Kw> {
        (t.kind == TokKind::Word).then(|| kw_of(t.text(self.text)))
    }

    fn is_name(&self, t: &Token) -> bool {
        match t.kind {
            TokKind::Quoted => true,
            TokKind::Word => {
                let w = t.text(self.text);
                kw_of(w) == Kw::Other
                    && ![
                        "end", "null", "true", "false", "asc", "desc", "and", "or", "in", "like",
                    ]
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(w))
            }
            _ => false,
        }
    }

    fn item_name(&self, item: &[Token]) -> Option<String> {
        let n = item.len();
        let last = item.last()?;
        let name = |t: &Token| self.column_name(t);
        if n >= 2 && self.kw(&item[n - 2]) == Some(Kw::As) && self.is_name(last) {
            return Some(name(last));
        }
        let chain = n % 2 == 1
            && item.iter().enumerate().all(|(k, t)| {
                if k % 2 == 0 {
                    self.is_name(t)
                } else {
                    t.is_punct(self.text, '.')
                }
            });
        if chain {
            return Some(name(last));
        }
        if n >= 2 && self.is_name(last) {
            let p = &item[n - 2];
            let operand = matches!(p.kind, TokKind::Quoted | TokKind::Number | TokKind::Str)
                || p.is_punct(self.text, ')')
                || (p.kind == TokKind::Word && self.is_name(p));
            if operand {
                return Some(name(last));
            }
        }
        None
    }
}
