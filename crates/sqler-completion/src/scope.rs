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

/// Words after `UPDATE` that are not the table (`UPDATE LOW_PRIORITY t`, `UPDATE OR REPLACE t`).
fn is_update_modifier(id: &Ident) -> bool {
    !id.quoted
        && ["low_priority", "ignore", "or", "rollback", "abort", "fail"]
            .iter()
            .any(|w| id.value.eq_ignore_ascii_case(w))
}

/// Whether `word` is a keyword the scanner gives a meaning to (`select`, `from`, `on`, ...).
pub(crate) fn is_scan_keyword(word: &str) -> bool {
    kw_of(word) != Kw::Other
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
        // `]]` is an escaped `]` in T-SQL; SQLite brackets never hold a `]`.
        inner.replace("]]", "]")
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
    /// Full mode: the branch (the part between set-operation keywords) of the enclosing query
    /// level that the token is in.
    pub branch: usize,
}

#[derive(Debug)]
struct Level {
    parent: Option<usize>,
    kind: LevelKind,
    /// The clause of the parent level when the `(` opened this one.
    open_clause: Clause,
    relations: Vec<ScopeRelation>,
    ctes: Vec<Cte>,
    /// The projection of every select list of this level (full mode only).
    outputs: Vec<String>,
    /// A from-item the scanner cannot name (a function call without alias).
    opaque: bool,
    /// Index in `relations` of the `INSERT INTO` target.
    target: Option<usize>,
    /// The branch being scanned: how many set-operation keywords came so far.
    branch: usize,
    /// The branch of each relation (parallel to `relations`).
    branches: Vec<usize>,
    /// The branch of the parent level that was current when this level opened.
    parent_branch: usize,
}

impl Level {
    fn new(parent: Option<usize>, kind: LevelKind, open_clause: Clause) -> Self {
        Self {
            parent,
            kind,
            open_clause,
            relations: Vec::new(),
            ctes: Vec::new(),
            outputs: Vec::new(),
            opaque: false,
            target: None,
            branch: 0,
            branches: Vec::new(),
            parent_branch: 0,
        }
    }
}

/// The result of scanning a statement.
#[derive(Debug)]
pub(crate) struct Scan {
    levels: Vec<Level>,
    pub top: Snapshot,
    /// Full mode: the state before each token, one per token.
    pub snaps: Vec<Snapshot>,
    /// Full mode: spans of CTEs and subqueries whose columns are not fully known.
    incomplete: Vec<Range<usize>>,
}

impl Scan {
    /// The relations of one level.
    pub(crate) fn relations(&self, level: usize) -> &[ScopeRelation] {
        self.levels.get(level).map_or(&[], |l| &l.relations)
    }

    /// The CTEs defined at one level.
    pub(crate) fn ctes(&self, level: usize) -> &[Cte] {
        self.levels.get(level).map_or(&[], |l| &l.ctes)
    }

    pub(crate) fn parent(&self, level: usize) -> Option<usize> {
        self.levels.get(level).and_then(|l| l.parent)
    }

    pub(crate) fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// The names the select lists of the level project.
    pub(crate) fn outputs(&self, level: usize) -> &[String] {
        self.levels.get(level).map_or(&[], |l| &l.outputs)
    }

    pub(crate) fn opaque(&self, level: usize) -> bool {
        self.levels.get(level).is_some_and(|l| l.opaque)
    }

    /// The index in `relations(level)` of the `INSERT INTO` target.
    pub(crate) fn target(&self, level: usize) -> Option<usize> {
        self.levels.get(level).and_then(|l| l.target)
    }

    /// The branch of relation `ri` of `level` (the part between set-operation keywords).
    pub(crate) fn rel_branch(&self, level: usize, ri: usize) -> usize {
        self.levels
            .get(level)
            .and_then(|l| l.branches.get(ri))
            .copied()
            .unwrap_or(0)
    }

    /// The enclosing level of `level` and the branch of it that `level` sits in.
    pub(crate) fn outer(&self, level: usize) -> Option<(usize, usize)> {
        let lv = self.levels.get(level)?;
        lv.parent.map(|p| (p, lv.parent_branch))
    }

    /// The relations of one branch of one level, with their indexes in `relations(level)`.
    pub(crate) fn branch_relations(
        &self,
        level: usize,
        branch: usize,
    ) -> impl Iterator<Item = (usize, &ScopeRelation)> {
        self.relations(level)
            .iter()
            .enumerate()
            .filter(move |(ri, _)| self.rel_branch(level, *ri) == branch)
    }

    /// The nearest enclosing level that is a statement or a subquery (the level itself if it is).
    pub(crate) fn query_level(&self, level: usize) -> usize {
        let mut l = level;
        while let Some(lv) = self.levels.get(l) {
            if matches!(
                lv.kind,
                LevelKind::Root | LevelKind::Sub | LevelKind::CteBody
            ) {
                break;
            }
            match lv.parent {
                Some(p) => l = p,
                None => break,
            }
        }
        l
    }

    /// The clause a token is in, looking through expression parentheses.
    pub(crate) fn effective_clause(&self, snap: &Snapshot) -> Clause {
        let mut clause = snap.clause;
        let mut level = snap.level;
        while clause == Clause::Expr {
            let Some(lv) = self.levels.get(level) else {
                break;
            };
            clause = lv.open_clause;
            match lv.parent {
                Some(p) => level = p,
                None => break,
            }
        }
        clause
    }

    /// Resolves a qualifier to a relation visible from `level`: by alias first, else by bare table
    /// or CTE name; the level's own relations before the enclosing ones. Returns the level that
    /// holds the relation.
    pub(crate) fn resolve_qualifier(
        &self,
        level: usize,
        branch: usize,
        qualifier: &Ident,
        dialect: Dialect,
    ) -> Option<(usize, usize, &ScopeRelation)> {
        for by_alias in [true, false] {
            let mut next = Some((level, branch));
            while let Some((l, b)) = next {
                for (ri, r) in self.branch_relations(l, b) {
                    let hit = if by_alias {
                        r.alias
                            .as_ref()
                            .is_some_and(|a| idents_match(dialect, qualifier, a))
                    } else {
                        match &r.source {
                            RelationSource::Table { name, .. } | RelationSource::Cte { name } => {
                                idents_match(dialect, qualifier, name)
                            }
                            RelationSource::Subquery { .. } => false,
                        }
                    };
                    if hit {
                        return Some((l, ri, r));
                    }
                }
                next = self.outer(l);
            }
        }
        None
    }

    /// The CTE `name` refers to, looking at `level` and the enclosing levels.
    pub(crate) fn find_cte(&self, level: usize, name: &Ident, dialect: Dialect) -> Option<&Cte> {
        let mut next = Some(level);
        while let Some(l) = next {
            if let Some(c) = self
                .ctes(l)
                .iter()
                .find(|c| idents_match(dialect, &c.name, name))
            {
                return Some(c);
            }
            next = self.parent(l);
        }
        None
    }

    /// Whether the CTE or subquery that starts at `start` exposes columns the scanner could not
    /// name completely (`*`, unnamed expressions, functions, `VALUES`).
    pub(crate) fn is_incomplete(&self, start: usize) -> bool {
        self.incomplete.iter().any(|r| r.start == start)
    }

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
    StraightJoin,
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
    let mut buf = [0u8; 16];
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
        b"straight_join" => Kw::StraightJoin,
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

/// Whether a `USING` would list relations (`DELETE FROM t USING a, b`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeleteUsing {
    /// Not a `DELETE`.
    Not,
    /// A `DELETE` where no `JOIN` or `USING` came yet.
    Open,
    /// A `DELETE` where a `JOIN` or `USING` came: `USING` is a join condition now.
    Closed,
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
    /// A CTE body just closed: until a `,` starts the next CTE, the words are CTE trailers.
    cte_done: bool,
    /// A PostgreSQL `SEARCH` / `CYCLE` clause follows the last CTE body.
    cte_gen: bool,
    delete: DeleteUsing,
    /// Index in the level's relations of the relation whose alias was just read.
    alias_rel: Option<usize>,
    /// For a parenthesis level that follows an alias: the relation its names rename (`AS q(a, b)`).
    alias_list_of: Option<usize>,
    /// Full mode: the projection of each select list so far, and whether it is fully named.
    lists: Vec<(Vec<String>, bool)>,
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
            cte_done: false,
            cte_gen: false,
            delete: DeleteUsing::Not,
            alias_rel: None,
            alias_list_of: None,
            lists: Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            clause: self.clause,
            fs: self.fs,
            kind: self.kind,
            level: self.level,
            branch: 0,
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
    full: bool,
    snaps: Vec<Snapshot>,
    incomplete: Vec<Range<usize>>,
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
    run(text, toks, prefix_start, skip_prefix, dialect, false)
}

/// Scans a whole statement: no prefix, nothing skipped. Also records the state before every
/// token, the projection of every select list and the CTEs and subqueries with unknown columns.
pub(crate) fn scan_all(text: &str, toks: &[Token], dialect: Dialect) -> Scan {
    run(text, toks, usize::MAX, false, dialect, true)
}

fn run(
    text: &str,
    toks: &[Token],
    prefix_start: usize,
    skip_prefix: bool,
    dialect: Dialect,
    full: bool,
) -> Scan {
    let mut s = Scanner {
        dialect,
        text,
        toks,
        levels: vec![Level::new(None, LevelKind::Root, Clause::Start)],
        stack: vec![Frame::new(0, LevelKind::Root, 0, 0)],
        snapshot: None,
        full,
        snaps: Vec::new(),
        incomplete: Vec::new(),
    };
    for (i, t) in toks.iter().enumerate() {
        if full && let Some(f) = s.stack.last() {
            let mut snap = f.snapshot();
            snap.branch = s.query_branch(snap.level);
            s.snaps.push(snap);
        }
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
            branch: 0,
        });
    while let Some(mut f) = s.stack.pop() {
        s.flush(&mut f);
        s.finish_level(&mut f);
    }
    Scan {
        levels: s.levels,
        top,
        snaps: s.snaps,
        incomplete: s.incomplete,
    }
}

impl Scanner<'_> {
    /// The branch of the enclosing query level that `level` is in now.
    fn query_branch(&self, level: usize) -> usize {
        let mut l = level;
        loop {
            let Some(lv) = self.levels.get(l) else {
                return 0;
            };
            let is_query = matches!(
                lv.kind,
                LevelKind::Root | LevelKind::Sub | LevelKind::CteBody
            );
            let Some(p) = lv.parent else {
                return lv.branch;
            };
            if is_query {
                return lv.branch;
            }
            if self.levels.get(p).is_some_and(|x| {
                matches!(
                    x.kind,
                    LevelKind::Root | LevelKind::Sub | LevelKind::CteBody
                )
            }) {
                return lv.parent_branch;
            }
            l = p;
        }
    }

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
                "," => self.comma(i),
                "." => self.dot(),
                _ => {}
            },
            TokKind::Str | TokKind::Number | TokKind::Comment => {}
        }
    }

    /// Registers a pending relation (without alias) when leaving the relation state.
    fn flush(&mut self, f: &mut Frame) {
        f.alias_rel = None;
        if matches!(f.fs, Fs::Name | Fs::AsKw) {
            if let Some(p) = f.pending.take() {
                self.finish_relation(f.level, f.clause, p, None);
            }
        } else {
            f.pending = None;
        }
    }

    fn finish_relation(
        &mut self,
        level: usize,
        clause: Clause,
        pending: Pending,
        alias: Option<Ident>,
    ) {
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
                if self.full {
                    self.incomplete.push(span.clone());
                }
                if alias.is_none() {
                    self.levels[level].opaque = true;
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
        let lv = &mut self.levels[level];
        if clause == Clause::Into && lv.target.is_none() {
            lv.target = Some(lv.relations.len());
        }
        lv.branches.push(lv.branch);
        lv.relations.push(ScopeRelation {
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
                Fs::Expect if f.clause == Clause::Update && is_update_modifier(&id) => {}
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
                        self.finish_relation(f.level, f.clause, p, Some(id));
                        f.alias_rel = self.levels[f.level].relations.len().checked_sub(1);
                    }
                    f.fs = Fs::Done;
                }
                // A word the scanner has no rule for after a from-item (`STRAIGHT_JOIN`-like
                // joins, `TABLESAMPLE`, `USE INDEX`, ...): what follows cannot be trusted.
                Fs::Done => self.levels[f.level].opaque = true,
            }
        } else if f.clause == Clause::With && !f.cte_as {
            if f.cte_done {
                // PostgreSQL `SEARCH ... SET seq` / `CYCLE ... SET c USING p` add generated
                // columns to the CTE: its columns are no longer fully known.
                let word = id.value.as_str();
                if !f.cte_gen
                    && !id.quoted
                    && (word.eq_ignore_ascii_case("search") || word.eq_ignore_ascii_case("cycle"))
                {
                    f.cte_gen = true;
                    if self.full
                        && let Some(c) = self.levels[f.level].ctes.last()
                    {
                        self.incomplete.push(c.span.clone());
                    }
                }
            } else if f.cte_name.is_none() {
                f.cte_name = Some(id);
            }
        }
        self.stack.push(f);
    }

    fn comma(&mut self, i: usize) {
        let new_cte = {
            let name = self
                .toks
                .get(i + 1)
                .is_some_and(|t| matches!(t.kind, TokKind::Word | TokKind::Quoted));
            name && self
                .toks
                .get(i + 2)
                .is_some_and(|t| t.is_punct(self.text, '(') || kw_of(t.text(self.text)) == Kw::As)
        };
        let Some(mut f) = self.stack.pop() else {
            return;
        };
        if f.clause == Clause::With && f.cte_done && (!f.cte_gen || new_cte) {
            f.cte_done = false;
            f.cte_gen = false;
            f.cte_name = None;
            f.cte_cols = None;
        }
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
        let open_clause = f.clause;
        let list_of = (kind == LevelKind::Expr
            && matches!(f.clause, Clause::From | Clause::Update)
            && f.fs == Fs::Done)
            .then_some(f.alias_rel)
            .flatten();
        self.stack.push(f);
        let level = self.levels.len();
        let mut lv = Level::new(Some(parent), kind, open_clause);
        lv.parent_branch = self.levels[parent].branch;
        self.levels.push(lv);
        let mut frame = Frame::new(level, kind, i, t.start);
        frame.alias_list_of = list_of;
        self.stack.push(frame);
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
        let child_incomplete = self.finish_level(&mut child);
        let Some(mut parent) = self.stack.pop() else {
            return;
        };
        if let Some(ri) = child.alias_list_of {
            self.apply_alias_list(parent.level, ri, child.open_idx, i);
        }
        match child.kind {
            LevelKind::Sub if parent.is_relation_clause() && parent.fs == Fs::Expect => {
                if self.full && child_incomplete {
                    self.incomplete.push(child.open_start..t.end);
                }
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
                    let declared = parent.cte_cols.take();
                    if self.full && declared.is_none() && child_incomplete {
                        self.incomplete.push(name.span.start..t.end);
                    }
                    let columns = declared.unwrap_or_else(|| self.projection(&child));
                    let span = name.span.start..t.end;
                    self.levels[parent.level].ctes.push(Cte {
                        name,
                        columns,
                        span,
                    });
                }
                parent.cte_as = false;
                parent.cte_done = true;
                parent.cte_gen = false;
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

    /// `FROM (SELECT ...) AS q(a, b)`: the names rename the columns of the relation. Anything but
    /// a plain list of names, or a relation that is not a subquery, makes the level opaque.
    fn apply_alias_list(&mut self, level: usize, ri: usize, open_idx: usize, close_idx: usize) {
        let inner = self.toks.get(open_idx + 1..close_idx).unwrap_or(&[]);
        let plain = inner.len() % 2 == 1
            && inner.iter().enumerate().all(|(n, t)| {
                if n % 2 == 0 {
                    matches!(t.kind, TokKind::Word | TokKind::Quoted)
                } else {
                    t.is_punct(self.text, ',')
                }
            });
        let names: Vec<String> = inner
            .iter()
            .step_by(2)
            .map(|t| self.column_name(t))
            .collect();
        let Some(lv) = self.levels.get_mut(level) else {
            return;
        };
        match lv.relations.get_mut(ri).map(|r| &mut r.source) {
            Some(RelationSource::Subquery { columns }) if plain => {
                let rest = columns.split_off(names.len().min(columns.len()));
                *columns = names;
                columns.extend(rest);
            }
            _ => lv.opaque = true,
        }
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
        // PostgreSQL `ROWS FROM (f(), g())`: a table function, not a table named `rows`.
        if k == Kw::From
            && next_paren
            && rel
            && f.fs == Fs::Name
            && matches!(&f.pending, Some(Pending::Parts(p))
                if matches!(p.as_slice(), [x] if !x.quoted && x.value.eq_ignore_ascii_case("rows")))
        {
            self.stack.push(f);
            return;
        }
        match k {
            Kw::Select => {
                self.flush(&mut f);
                f.clause = Clause::Select;
                f.sel = Some((i + 1, None));
            }
            Kw::From if is_distinct_from => {}
            // `SELECT STRAIGHT_JOIN ...` is a select modifier; between from-items it is a join.
            Kw::StraightJoin if !(rel || f.clause == Clause::On) => {}
            Kw::From | Kw::Join | Kw::StraightJoin => {
                self.flush(&mut f);
                f.clause = Clause::From;
                f.fs = Fs::Expect;
                if k != Kw::From && f.delete == DeleteUsing::Open {
                    f.delete = DeleteUsing::Closed;
                }
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
            Kw::Using if f.delete == DeleteUsing::Open && f.clause == Clause::From => {
                // PostgreSQL `DELETE FROM t USING a, b`: a list of relations.
                self.flush(&mut f);
                f.delete = DeleteUsing::Closed;
                f.fs = Fs::Expect;
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
                self.levels[f.level].branch += 1;
                self.record_list(&mut f);
                f.sel = None;
            }
            Kw::Set if f.clause == Clause::With && f.cte_done => {}
            Kw::Set => self.switch(&mut f, Clause::Set),
            Kw::Values => self.switch(&mut f, Clause::Values),
            Kw::Returning => self.switch(&mut f, Clause::Returning),
            Kw::With if f.clause == Clause::Start || f.clause == Clause::Into => {
                // `INSERT INTO t (cols) WITH s AS (...) SELECT ...`: the CTEs follow the target.
                self.flush(&mut f);
                f.clause = Clause::With;
                f.cte_name = None;
                f.cte_cols = None;
                f.cte_as = false;
                f.cte_done = false;
                f.cte_gen = false;
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
            Kw::Delete if at_start => {
                f.clause = Clause::Delete;
                f.delete = DeleteUsing::Open;
            }
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

    /// Full mode: appends the current select list of `f` to its lists.
    fn record_list(&self, f: &mut Frame) {
        if self.full && f.sel.is_some() {
            let list = self.projection_full(f);
            f.lists.push(list);
        }
    }

    /// Full mode: stores the projection of the level's select lists as its outputs. Returns
    /// whether the columns of the level are not fully known: some select item is unnamed or a
    /// `*`, the level has no select list, or its select lists (set operations) name different
    /// columns.
    fn finish_level(&mut self, f: &mut Frame) -> bool {
        if !self.full {
            return false;
        }
        self.record_list(f);
        let lists = std::mem::take(&mut f.lists);
        let mut incomplete = lists.is_empty();
        if let Some(first) = lists.first() {
            incomplete = lists.iter().any(|l| !l.1 || l.0 != first.0);
        }
        if let Some(lv) = self.levels.get_mut(f.level) {
            lv.outputs = lists.into_iter().flat_map(|l| l.0).collect();
        }
        incomplete
    }

    /// The column names a select list projects.
    fn projection(&self, f: &Frame) -> Vec<String> {
        self.projection_full(f).0
    }

    /// The column names a select list projects, and whether every item is named (no `*`).
    fn projection_full(&self, f: &Frame) -> (Vec<String>, bool) {
        let Some((start, end)) = f.sel else {
            return (Vec::new(), false);
        };
        let end = end.unwrap_or(self.toks.len());
        let Some(toks) = self.toks.get(start..end) else {
            return (Vec::new(), false);
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
        let mut complete = true;
        let names = items
            .into_iter()
            .filter_map(|it| {
                let name = self.item_name(it);
                complete &= name.is_some();
                name
            })
            .collect();
        (names, complete)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn full(sql: &str) -> (Scan, Vec<Token>) {
        let toks: Vec<Token> = lex(sql, Dialect::Postgres)
            .into_iter()
            .filter(|t| t.kind != TokKind::Comment)
            .collect();
        (scan_all(sql, &toks, Dialect::Postgres), toks)
    }

    #[test]
    fn one_snapshot_per_token() {
        for sql in [
            "",
            "SELECT a, (SELECT 1) FROM t WHERE x IN (1, 2)",
            "WITH c AS (SELECT 1) SELECT * FROM c",
            "SELECT ((((",
        ] {
            let (scan, toks) = full(sql);
            assert_eq!(scan.snaps.len(), toks.len(), "{sql}");
        }
    }

    #[test]
    fn snapshots_are_taken_before_the_token() {
        let sql = "SELECT a FROM t WHERE b";
        let (scan, toks) = full(sql);
        let clause_at = |w: &str| {
            let i = toks.iter().position(|t| t.text(sql) == w).unwrap();
            scan.snaps[i].clause
        };
        assert_eq!(clause_at("a"), Clause::Select);
        assert_eq!(clause_at("FROM"), Clause::Select);
        assert_eq!(clause_at("t"), Clause::From);
        assert_eq!(clause_at("b"), Clause::Where);
    }

    #[test]
    fn outputs_cover_every_select_list() {
        let (scan, _) = full("SELECT a, b AS c FROM t UNION SELECT x, y FROM u ORDER BY c");
        assert_eq!(scan.outputs(0), ["a", "c", "x", "y"]);
        let (scan, _) = full("SELECT * FROM (SELECT id AS k FROM t) s");
        assert_eq!(scan.outputs(1), ["k"]);
        assert_eq!(scan.outputs(0), Vec::<String>::new().as_slice());
    }

    #[test]
    fn incomplete_projections_are_recorded() {
        let sql = "WITH a AS (SELECT id FROM t), b AS (SELECT * FROM t), c(x) AS (SELECT 1), \
                   d AS (SELECT 1 AS k UNION SELECT 2 AS j), e AS (VALUES (1)) \
                   SELECT 1 FROM (SELECT id FROM t) s1, (SELECT count(*) FROM t) s2, \
                   generate_series(1, 2) g";
        let (scan, _) = full(sql);
        let start = |w: &str| sql.find(w).unwrap();
        assert!(!scan.is_incomplete(start("a AS")));
        assert!(scan.is_incomplete(start("b AS")));
        assert!(!scan.is_incomplete(start("c(x)")), "declared columns");
        assert!(
            scan.is_incomplete(start("d AS")),
            "set operation names differ"
        );
        assert!(scan.is_incomplete(start("e AS")));
        assert!(!scan.is_incomplete(start("(SELECT id FROM t) s1")));
        assert!(scan.is_incomplete(start("(SELECT count")));
        assert!(scan.is_incomplete(start("generate_series")));
    }

    #[test]
    fn levels_expose_kind_clause_and_target() {
        let (scan, toks) = full("INSERT INTO t (a) SELECT x FROM s WHERE y IN (1, 2)");
        assert_eq!(scan.target(0), Some(0));
        assert_eq!(scan.relations(0).len(), 2);
        let sql = "INSERT INTO t (a) SELECT x FROM s WHERE y IN (1, 2)";
        let two = toks.iter().position(|t| t.text(sql) == "2").unwrap();
        let snap = scan.snaps[two];
        assert_eq!(snap.kind, LevelKind::Expr);
        assert_eq!(scan.effective_clause(&snap), Clause::Where);
        assert_eq!(scan.query_level(snap.level), 0);
        let a = toks.iter().position(|t| t.text(sql) == "a").unwrap();
        assert_eq!(scan.snaps[a].kind, LevelKind::InsertCols);
    }

    #[test]
    fn delete_using_lists_relations() {
        let (scan, _) = full("DELETE FROM t USING a, b x WHERE t.id = a.id");
        let names: Vec<_> = scan
            .relations(0)
            .iter()
            .filter_map(|r| r.visible_name().map(|i| i.value.as_str()))
            .collect();
        assert_eq!(names, ["t", "a", "x"]);
        // A join's USING stays a join condition.
        let (scan, _) = full("DELETE FROM t JOIN a USING (id)");
        assert_eq!(scan.relations(0).len(), 2);
    }

    #[test]
    fn unnamed_function_from_items_make_the_level_opaque() {
        let (scan, _) = full("SELECT * FROM unnest(arr)");
        assert!(scan.opaque(0));
        let (scan, _) = full("SELECT * FROM unnest(arr) u");
        assert!(!scan.opaque(0));
    }
}
