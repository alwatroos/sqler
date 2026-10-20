# 0023 Completion engine: own lexer, schema source trait, ranking

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The SQL editor (T10) needs schema-aware completion while the user types, so the text it analyses is almost always incomplete: unterminated strings, a dangling `FROM`, a half-typed `u.`. A request must answer in well under a frame (budget p95 16 ms on a 5,000-table schema). The engine must work for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite`, and be testable without a database. `sqler-completion` is an L2 crate: pure, no async, no I/O, and it may depend on `sqler-core` and `sqler-sql` only, never on `sqler-catalog` (AGENTS.md §2). AGENTS.md §8 requires an ADR before a new public API.

## Decision drivers

- Never fail on bad input: every byte sequence and every offset must produce a result.
- Work on incomplete SQL; text after the cursor still matters (`SELECT u.| FROM users u`).
- Per-dialect quoting, case rules, keywords and functions.
- No new dependency, no C code in a pure crate.
- Reuse for hover and diagnostics (T7) without a second parser.

## Considered options

1. Hand-written total lexer plus a token-level clause tracker.
2. The `sqlparser` tokenizer and parser.
3. `tree-sitter` with a SQL grammar.

## Decision outcome

Chosen: option 1.

`sqlparser` errors on unterminated literals and on incomplete statements, and its parser cannot return a partial tree at the cursor. `tree-sitter` recovers from errors but adds a C grammar dependency to a pure L2 crate, and the grammar does not model the three dialects separately. A lexer that never fails, with a small clause tracker on top, gives the context and scope needed here at a few microseconds per statement.

**Lexer.** `lex(text, dialect)` is total: unterminated strings, quoted identifiers and comments run to the end of input and carry an `unterminated` flag. Tokens have byte spans. Rules per dialect:

| Dialect | Quoted identifiers | Strings | Comments |
| ------- | ------------------ | ------- | -------- |
| `Postgres` | `"id"` | `'..''..'`, `E'..\'..'`, `$tag$..$tag$` | `--`, nested `/* */` |
| `MySql` | `` `id` `` | `'..'` and `"..."` with `\` escapes | `#`, `-- `, `/* */` |
| `Sqlite` | `"id"`, `` `id` ``, `[id]` | `'..''..'` | `--`, `/* */` |

**Pipeline.** Statement under the cursor (split at `;` outside strings and comments) → clause context from the tokens before the prefix → scope over the whole statement → candidates → filter and rank. Details: [Completion engine](../architecture/completion.md).

**Public API** (offsets are UTF-8 byte offsets into `text`; structs and enums are `#[non_exhaustive]` except two-state enums):

```rust
pub struct CompletionRequest<'a> { pub text: &'a str, pub offset: usize, pub dialect: Dialect, pub limit: usize }
pub fn complete(req: &CompletionRequest<'_>, schema: &dyn SchemaSource) -> Completions;
pub fn analyze(text: &str, offset: usize, dialect: Dialect) -> Analysis;
pub struct Completions { pub items: Vec<CompletionItem>, pub replace: Range<usize>, pub context: ContextKind, pub requests: Vec<SchemaRequest> }
pub struct CompletionItem { pub label: String, pub kind: ItemKind, pub insert_text: String, pub format: InsertFormat, pub detail: Option<String>, pub score: u32 }
pub enum ItemKind { Schema, Table, View, MaterializedView, Cte, Alias, Column, Function, Keyword, Snippet }
pub enum InsertFormat { Plain, Snippet }
pub enum ContextKind { None, StatementStart, Relation, RelationTail, Expression, Qualified, InsertColumns }
pub enum SchemaRequest { Relations { schema: Option<String> }, Columns { schema: Option<String>, relation: String } }
```

`CompletionRequest::new(text, offset, dialect)` sets `limit` to 200. `Snippet` text uses LSP tab stops (`$1`, `$0`). `replace` is the byte range the chosen item replaces; inside an open quoted identifier it starts at the quote. `analyze` needs no schema and is the base for T7.

**Analysis types.**

```rust
pub struct Analysis { pub statement: Range<usize>, pub context: ContextKind, pub prefix: Range<usize>, pub qualifier: Vec<Ident>, pub scope: Scope }
pub struct Scope { pub ctes: Vec<Cte>, pub relations: Vec<ScopeRelation>, pub local: usize } // Default
impl ScopeRelation { pub fn visible_name(&self) -> Option<&Ident>; }
impl Ident { pub fn new(value: impl Into<String>, quoted: bool) -> Self; } // span 0..0
pub struct ScopeRelation { pub alias: Option<Ident>, pub source: RelationSource, pub span: Range<usize> }
pub enum RelationSource { Table { schema: Option<Ident>, name: Ident }, Cte { name: Ident }, Subquery { columns: Vec<String> } }
pub struct Cte { pub name: Ident, pub columns: Vec<String>, pub span: Range<usize> }
pub struct Ident { pub value: String, pub quoted: bool, pub span: Range<usize> }
impl Scope { pub fn resolve(&self, qualifier: &Ident, dialect: Dialect) -> Option<&ScopeRelation>; }
```

`Ident::value` is unquoted with doubled quotes undone. `Scope::relations` lists the cursor level first, then enclosing levels; `Scope::local` is how many of the leading relations belong to the cursor's own query level (T7 needs it to tell local from outer names). `Scope` implements `Default`. `visible_name` is the alias, else the table or CTE name (`None` for a subquery without alias). `resolve` matches an alias first, then a bare relation or CTE name, using the dialect's identifier rules. CTE references follow the same rules, so on `PostgreSQL` `"Recent"` and `recent` are different CTEs. `Cte::columns` and `RelationSource::Subquery::columns` hold the names as the dialect resolves them (see Identifiers and quoting), not as written.

**Schema abstraction.** The engine reads the schema only through a trait defined over `sqler-core` types, so tests use `MemorySchema` and T10 adapts the `sqler-catalog` cache:

```rust
pub enum Loadable<T> { Loaded(T), NotLoaded }
pub struct Relation<'a> { pub schema: Option<&'a str>, pub name: &'a str, pub kind: ObjectKind }
impl<'a> Relation<'a> { pub fn new(schema: Option<&'a str>, name: &'a str, kind: ObjectKind) -> Self; } // the struct is non_exhaustive
pub trait SchemaSource {
    fn schemas(&self) -> Box<dyn Iterator<Item = &str> + '_>;
    fn search_path(&self) -> &[String];
    fn relations(&self, schema: Option<&str>) -> Loadable<Box<dyn Iterator<Item = Relation<'_>> + '_>>;
    fn columns(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ColumnInfo]>;
}
```

`relations(None)` means the search path; an implementation that has not loaded one of the search-path schemas returns `NotLoaded` (the trait has no partial state; `MemorySchema` does so). `columns` takes the exact stored names; case handling is the engine's job. `MemorySchema` is the in-memory implementation: `MemorySchema::new(impl IntoIterator<Item = impl Into<String>>)` takes the search path, and `add_schema`, `add_table` and `mark_relations_not_loaded` return `&mut Self` so calls chain. `NotLoaded` is not an error: the engine adds a `SchemaRequest` to `Completions::requests` and returns what it has. The caller loads the data and asks again. `Loadable` has two states and is not `non_exhaustive`.

**Hover and diagnostics (shape fixed here, implemented by T7, not by T6).**

```rust
pub fn hover(req: &HoverRequest, schema: &dyn SchemaSource) -> Option<Hover>;
pub struct Hover { pub range: Range<usize>, pub target: HoverTarget }
pub enum HoverTarget { Relation(..), Column { relation: .., info: ColumnInfo, references: Option<ForeignKey> } }
pub fn diagnose(text: &str, dialect: Dialect, schema: &dyn SchemaSource) -> Vec<Diagnostic>;
pub struct Diagnostic { pub range: Range<usize>, pub severity: Severity, pub code: DiagnosticCode, pub message: String }
pub enum Severity { Error, Warning, Info }
pub enum DiagnosticCode { UnknownRelation, UnknownColumn, UnknownQualifier, AmbiguousColumn }
```

Foreign key data arrives through a future trait method with a default, `fn foreign_keys(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ForeignKey]>`, defaulting to `Loaded(&[])`, so adding it does not break existing `SchemaSource` implementations. T7 may refine the exact field types of `HoverTarget::Relation`; the names above are the contract.

**Dialect data.** Static tables per dialect: keywords (reserved ones flagged for quoting), about 60 to 120 built-in functions with a short signature as detail (aggregate, string, date, math, JSON), and five snippets (`sel`, `ins`, `upd`, `del`, `cte`). The match on `Dialect` has a `PostgreSQL` fallback, because `Dialect` is `#[non_exhaustive]`. User-defined functions from the schema are not offered yet.

**Identifiers and quoting.** Matching: `PostgreSQL` folds an unquoted name to lower case and compares it exactly, a quoted name is compared exactly, with a case-insensitive fallback so lenient typing still completes; `MySQL` and `SQLite` compare case-insensitively (Unicode lower case). Inserted names are quoted only when needed: `PostgreSQL` unless the name matches `[a-z_][a-z0-9_$]*` and is not reserved; the others unless it matches `[A-Za-z_][A-Za-z0-9_$]*` and is not reserved. Quoting itself is `sqler_sql::quote_identifier` (ADR-0015); a name it rejects (empty, or containing NUL) drops the candidate, it is never inserted raw. Names that come from the SQL text (aliases, bare table names, CTE names, the qualifier of an ambiguous column, and the output column names of CTEs and subqueries, which are stored folded at scan time) are inserted the way the dialect resolves them: on `PostgreSQL` an unquoted name is folded to lower case first (`FROM USERS U` offers `u`, not `"U"`), a quoted name keeps its case and is inserted quoted when it needs it (`"Recent"`). `MySQL` and `SQLite` keep such names as written: table-name case rules on `MySQL` depend on the server and file system, and `SQLite` is case-insensitive, so recasing would help nowhere. Keyword case follows the typed prefix: an all-lower-case prefix gives lower case, otherwise upper case.

**Ranking.** Match classes, best first: exact, case-insensitive prefix, word-segment prefix (`uid` matches `user_id`; split on `_` and on a case change), subsequence. No match drops the item; an empty prefix keeps all. Within a class the kind rank depends on the context:

- Expression: column at the cursor level, column of an outer level, alias or CTE, function, keyword, snippet. Snippets are offered in an expression only after something is typed.
- Relation: CTE, table, view and materialized view, schema, keyword.
- Qualified: columns or relations only.
- StatementStart: keywords and snippets.

Keywords, functions and snippets never match fuzzily (subsequence): `us` does not offer `VALUES`; only names from the schema and the scope do. Statement-start and relation-tail keywords keep list order (most useful first) for an empty prefix. Function items insert `name()` as plain text when the function takes no arguments (`now`) and `name($1)` as a snippet otherwise. The full order is (match class, kind rank, subsequence gap, label length, label, candidate sequence). `score` is strictly decreasing along that order, and only the top `limit` items are sorted (`select_nth_unstable_by`, then a sort of that part).

**Statements and limits.** Statements split at every `;` outside strings and comments; `BEGIN ... END` bodies are not special-cased. Out-of-range offsets are clamped to the text length and floored to a char boundary.

**Benchmark.** `cargo bench -p sqler-completion --bench completion` on a 5,000-table schema (plus 20 schemas), eight scenarios, std-only. Budget: `complete_p95_ns` and `worst_p95_ns` below 16 ms; regression rule: the minimum `complete_p95_ns` of 3 in-process repetitions may exceed the base commit's by at most 25 % + 250 us; run by `bench.yml`. Results: [benchmarks](../benchmarks.md#completion).

## Consequences

- Positive: completion works on any text, adds no dependency, is pure and fast, and is fully testable with `MemorySchema`. The same `analyze` result serves hover and diagnostics.
- Positive: `NotLoaded` gives the editor a precise list of what to load, so completion never blocks on introspection.
- Negative: the clause tracker is heuristic. Unusual syntax (vendor extensions, procedure bodies, deeply nested expressions) can give a weaker context than a real parser would. The failure mode is fewer or less ranked items, never a panic or an error.
- Negative: two keyword and function lists per dialect to maintain by hand, with no per-version variants.
- Negative: the engine p95 is not the editor p95; T10 measures end to end.
- A future switch to a real parser would be a new ADR; the public API above does not expose the lexer.
