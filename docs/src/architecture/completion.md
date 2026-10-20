# Completion engine

`sqler-completion` (L2) answers "what can be typed at this cursor position" for the SQL editor (T10). It is pure and synchronous: no async, no I/O, no UI. Decision and full API: [ADR-0023](../adr/0023-completion-engine.md). Numbers: [benchmarks](../benchmarks.md#completion).

## Pipeline

```text
  text + byte offset + Dialect
        |
        v
  lexer          total, byte spans, never fails
        |
        v
  statement      split at ';' outside strings/comments; the one under the cursor
        |
        v
  context        clause tracker with a paren-depth stack, tokens before the prefix
        |
        v
  scope          CTEs, tables, aliases, subqueries of the WHOLE statement
        |
        v
  candidates     SchemaSource + static keyword/function/snippet tables
        |
        v
  rank           prefix, word-segment, fuzzy; kind rank per context; top `limit`
        |
        v
  Completions { items, replace, context, requests }
```

- **Lexer.** One pass per dialect rules (quotes, dollar quotes, nested comments, `[id]`). An unterminated string or comment runs to the end and is flagged; a cursor inside one gives context `None` and no items.
- **Statement.** The offset is clamped to the text and floored to a char boundary. Only the statement under the cursor is analysed.
- **Context.** `StatementStart`, `Relation` (after `FROM`, `JOIN`, `INTO`, `UPDATE`, `TABLE`, or a comma in a FROM clause), `RelationTail`, `Expression` (select list, `WHERE`, `ON`, `USING (`, `GROUP BY`, `HAVING`, `ORDER BY`, `SET`, `RETURNING`), `Qualified` (after `ident .`), `InsertColumns`, `None`.
- **Scope.** Built over the whole statement, so text after the cursor counts: `SELECT u.| FROM users u` completes the columns of `users`. Relations are listed cursor level first, then enclosing levels; `Scope::local` counts the leading ones that belong to the cursor's own query level, and `ScopeRelation::visible_name()` is the alias, else the table or CTE name. `Scope` is `Default`, and `Ident::new` builds an identifier for lookups. Subquery aliases expose their projected names; `*` contributes nothing. CTE and subquery column names are stored the way the dialect resolves them (`PostgreSQL` folds unquoted names to lower case), and CTE references match by the dialect's identifier rules.
- **Candidates.** `Relation`: CTEs, relations on the search path, schemas. `schema.`: that schema's relations. `alias.` or `relation.`: the columns of the resolved relation (CTE and subquery: declared or projected names). `Expression`: columns of all scope relations (a name found in several relations is inserted as `qualifier.col`), aliases, functions, keywords, and snippets once something is typed. Functions insert `name()` (plain) when they take no arguments and `name($1)` (snippet) otherwise. Keywords, functions and snippets match by prefix or word segment only, never fuzzily.
- **Rank.** Match class first, then kind rank for the context, then a gap measure, label length and lower-case label.

`analyze(text, offset, dialect)` runs the first four stages without a schema. Hover and diagnostics reuse the same lexer and scanner (see [Hover and diagnostics](#hover-and-diagnostics)).

## Schema access

The engine never sees `sqler-catalog`. It reads through the `SchemaSource` trait, defined over `sqler-core` types (`ObjectKind`, `ColumnInfo`):

| Method | Meaning |
| ------ | ------- |
| `schemas()` | schema names |
| `search_path()` | schemas that unqualified names resolve in, in order |
| `relations(schema)` | `Loadable` list of tables and views; `None` = search path |
| `columns(schema, relation)` | `Loadable` columns, exact stored names |
| `foreign_keys(schema, relation)` | `Loadable` foreign keys; has a default (loaded, none), so existing implementations keep compiling |

`Relation::new` builds a listed relation (the struct is `non_exhaustive`). `relations(None)` is `NotLoaded` when any search-path schema is not loaded. `MemorySchema` implements it for tests and the benchmark: `new` takes the search path, and `add_schema`, `add_table`, `add_foreign_key` and `mark_relations_not_loaded` return `&mut Self` for chaining. T10 adapts the `sqler-catalog` cache (its lazily loaded nodes map to `Loadable`) to the trait, so the dependency points from the UI layer down to both crates.

## Not loaded data

The catalog loads lazily (ADR-0014), so a relation list or a column list may not be there yet. The source then returns `Loadable::NotLoaded`. The engine does not fail: it completes with what it has and appends a `SchemaRequest` (`Relations { schema }` or `Columns { schema, relation }`) to `Completions::requests`. The editor asks the catalog to load those, and repeats the request when the data arrives.

## Dialects

Keywords, built-in functions with a signature detail, and the snippets `sel`, `ins`, `upd`, `del`, `cte` are static tables per dialect. Identifier matching and quoting follow the dialect (see ADR-0023); inserted names go through `sqler_sql::quote_identifier` only when quoting is needed, and a name it rejects drops the candidate instead of being inserted raw. Names written in the SQL itself are inserted the way the dialect resolves them: on `PostgreSQL` `FROM USERS U` offers `u`, and a quoted `"Recent"` stays quoted; `MySQL` and `SQLite` keep them as written.

## Limits

- The context detection is token based and heuristic; odd syntax lowers quality, never causes an error.
- Statements split at every `;` outside strings and comments, so procedure bodies are not special-cased.
- No user-defined functions from the schema yet.
- Diagnostics are heuristic and conservative (see below): they find typos in names, not every semantic error.

## Hover and diagnostics

Both are pure functions over the same lexer and scope scanner as completion (no second parser, no dependency on `sqler-sql`'s statement splitter). The scanner has a "full" mode that records the state (query level, clause, level kind) before every token, the names each select list projects, which CTEs and subqueries have columns it cannot name completely (`*`, unnamed expressions, function calls, `VALUES`, set operations with different names), and whether a level has a from-item it cannot address. Completion output is unchanged by it. `DELETE ... USING a, b` is now read as a relation list.

```rust
pub fn hover(req: &HoverRequest, schema: &dyn SchemaSource) -> Option<Hover>;
pub fn diagnose(text: &str, dialect: Dialect, schema: &dyn SchemaSource) -> Vec<Diagnostic>;
```

`Hover { range, target }` where `target` is `Relation(RelationInfo)`, `Column { relation, info, references }` (the `ColumnInfo` with type, native type and nullability, and the `ForeignKey` the column takes part in, a single-column key first) or `DerivedColumn { relation, name }` (a column of a CTE or subquery: only the name is known). `RelationInfo { schema, name, kind }` has the stored names; `kind` is `Table`, `View`, `MaterializedView`, `Cte` or `Subquery`. `Diagnostic { range, severity, code, message }`, sorted by position; the range is the identifier token, quotes included. All offsets are UTF-8 byte offsets; a hover offset is clamped like a completion offset.

Refinements of the shape fixed in ADR-0023, all additive: `HoverRequest` has the fields `text`, `offset` and `dialect`; `RelationInfo`/`RelationKind` and `DerivedColumn` carry what hover shows; `DiagnosticCode::UnknownSchema` reports a schema before the table; `SchemaSource::foreign_keys` has a default; `MemorySchema::add_foreign_key` is a no-op for a table that does not exist. No ADR was needed.

### Resolution

- Hover. The token under the cursor (a word or quoted name containing the offset, else one that ends at it). It is a CTE name, the name or alias of a relation in the statement (relation hover; a schema part gives `None`), or part of a dotted chain in an expression clause: a qualifier gives its relation, the last part the column of that relation, an unqualified name the first relation in scope (own level first, then outer levels) that has it. The names in `INSERT INTO t (a, b)` are columns of `t`. Keywords, functions, literals, comments, and columns of relations whose columns are not loaded give `None`. Matching is lenient: a strict dialect match first, a case-insensitive one as fallback.
- Diagnostics. Strict matching: `PostgreSQL` folds an unquoted name to lower case and compares a quoted one exactly (`"Users"` is not `users`, `USERS` is); `MySQL` and `SQLite` compare case-insensitively. A relation is looked up in its schema (qualified) or on the search path; the schema is checked first. Relations are checked wherever they appear in the statement, references only in expression clauses (select list, `WHERE`, `ON`, `GROUP BY`, `HAVING`, `ORDER BY`, `SET`, `RETURNING`) and in `INSERT` column lists. A qualified `q.c` resolves `q` the way completion does (alias first, then the bare table or CTE name, own level first); `s.t.c` needs a table `s.t` in the statement; a from-item written without a schema (`FROM users`) also matches `public.users.id`, and then the column is not judged. `RETURNING` of an `INSERT` resolves against the insert target only; of `UPDATE ... FROM` and `DELETE ... USING` against every from-item. An unqualified name must be in exactly one relation of the own level (more is `AmbiguousColumn`, a warning, unless `USING` or `NATURAL` is in the statement), else in an outer level. The target of `INSERT INTO t ... SELECT` is not in scope of the `SELECT`.
- Cost. `diagnose` keeps a per-call index of each schema's relation names (sorted by a hash of the folded name), so the 2,000-statement script of the benchmark does one hash lookup per relation instead of scanning 5,000 names; hover looks names up linearly (one lookup).

### Suppression rules

A false positive costs more than a missed typo, so a name is reported only when the schema source can say it does not exist. The rules, each with a regression test in `tests/diagnostics.rs`, `tests/adversarial.rs` or `tests/never_flag.rs` (the last holds a generated corpus of valid statements that must stay clean):

| Situation | Result |
| --------- | ------ |
| Statement does not start with `SELECT`, `WITH`, `INSERT`, `REPLACE`, `UPDATE`, `DELETE`, `TABLE`, `VALUES`, `EXPLAIN` or `(` (DDL, `SET`, `SHOW`, ...) | skipped |
| Relation list of a schema not loaded, no schemas at all, empty search path | no relation or schema diagnostics |
| Columns of a relation not loaded | its columns are never flagged; unqualified names in a level with such a relation are not flagged |
| System schemas (`pg_catalog`, `information_schema`, `pg_toast`, `pg_temp`, `pg_temp_N`, `pg_toast_temp_N`; MySQL `mysql`, `performance_schema`, `sys`; SQLite `main`, `temp`) and relations (`pg_*`, `sqlite_*`, `dual`) | never flagged |
| Table or view created, altered or renamed anywhere in the text (`CREATE [TEMP\|VIRTUAL] TABLE\|VIEW`, `ALTER TABLE`, `RENAME TO`, `RENAME TABLE`, `SELECT ... INTO`), schema from `CREATE SCHEMA\|DATABASE` or `ATTACH ... AS` | never flagged, columns unknown |
| Implicit system columns (PostgreSQL `ctid`, `xmin`, `xmax`, `cmin`, `cmax`, `tableoid`, `oid`; SQLite `rowid`, `_rowid_`, `oid`; MySQL `_rowid`), qualified or not | not flagged |
| `ORDER BY` after a set operation (`UNION`, `INTERSECT`, `EXCEPT`) | not checked (it resolves against output names and positions). Each branch otherwise resolves names against its own relations and the outer levels only, so ambiguity, unknown columns and unknown qualifiers are judged per branch |
| Name left of `=` in `SET` (or in `SET (a, b) =`) | no `AmbiguousColumn` (it means the update target); the right side is resolved normally |
| PostgreSQL `SEARCH ... SET` / `CYCLE ... SET ... USING` after a CTE | the CTE's columns become unknown (the clause adds generated columns) |
| `LOCK IN SHARE MODE`, `FOR SHARE` | `SHARE` and `MODE` are not columns |
| A word the scanner has no rule for after a from-item (`TABLESAMPLE`, `USE INDEX`, `INDEXED BY`, a join keyword it does not know); `AS q(a, b)` on anything but a subquery | the level is uncertain |
| MySQL select modifiers (`SQL_CALC_FOUND_ROWS`, `DISTINCTROW`, ...), `GROUPING SETS`, the charset in `CONVERT(x USING cs)`, `U&'..'`, `SELECT ... INTO name`, `ROWS FROM (...)`, `UPDATE LOW_PRIORITY\|IGNORE\|OR REPLACE t` | not references |
| CTE or subquery with `*`, an unnamed expression, a function call, `VALUES` or differing set-operation names, and no declared column list | columns unknown |
| Function in `FROM` without alias, a parenthesised join, an unaliased subquery | the level is uncertain: unqualified names and unknown qualifiers are not flagged |
| Recursive CTE referring to itself | its name is a CTE, not a table |
| `ORDER BY`, `GROUP BY`, `HAVING` name equal to an output alias | not flagged |
| Chain followed by `(` (function call), word followed by a string (`DATE '..'`), preceded by `::`, `AS`, `@`, `:`, `?`, `$`, `OVER`, `COLLATE` or an operand (implicit alias), `EXTRACT(year FROM ..)`, unterminated quoted names, 4+ part chains, `t.*`, member access `(x).f` | not a reference |
| Words that are keywords, reserved words, function names, type names or date parts | not flagged when unqualified and not found in scope |
| Whole-row reference (`to_jsonb(u)`), `excluded`, `new`, `old`, a schema name used as qualifier | not flagged |
| Everything after `ON CONFLICT` or `ON DUPLICATE` | skipped |
| SQLite `"double quoted"` name that is not a column | a string literal there, not flagged |
| Statement with no relation at all (`SELECT foo`) | not flagged |
| A `CREATE\|ALTER PROCEDURE\|FUNCTION\|TRIGGER\|EVENT` definition | nothing is reported inside it, whatever the code (parameters, `DECLARE`d locals, `NEW`/`OLD`, and relation typos in the body are all silent). Under a custom `DELIMITER` it runs to the next `DELIMITER` line or the end of the text; otherwise it ends at its own closing `;` outside `BEGIN ... END` / `CASE ... END` nesting (a dollar-quoted body is one token), so later statements are still checked. `DELIMITER` counts only as the first word of a line; `tests/routines.rs`, `tests/diagnostics.rs` |

Because the scanner is heuristic, unknown syntax produces fewer diagnostics, never more.

