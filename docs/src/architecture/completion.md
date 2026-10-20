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

`analyze(text, offset, dialect)` runs the first four stages without a schema. It is the base for hover and diagnostics (T7).

## Schema access

The engine never sees `sqler-catalog`. It reads through the `SchemaSource` trait, defined over `sqler-core` types (`ObjectKind`, `ColumnInfo`):

| Method | Meaning |
| ------ | ------- |
| `schemas()` | schema names |
| `search_path()` | schemas that unqualified names resolve in, in order |
| `relations(schema)` | `Loadable` list of tables and views; `None` = search path |
| `columns(schema, relation)` | `Loadable` columns, exact stored names |

`Relation::new` builds a listed relation (the struct is `non_exhaustive`). `relations(None)` is `NotLoaded` when any search-path schema is not loaded. `MemorySchema` implements it for tests and the benchmark: `new` takes the search path, and `add_schema`, `add_table` and `mark_relations_not_loaded` return `&mut Self` for chaining. T10 adapts the `sqler-catalog` cache (its lazily loaded nodes map to `Loadable`) to the trait, so the dependency points from the UI layer down to both crates.

## Not loaded data

The catalog loads lazily (ADR-0014), so a relation list or a column list may not be there yet. The source then returns `Loadable::NotLoaded`. The engine does not fail: it completes with what it has and appends a `SchemaRequest` (`Relations { schema }` or `Columns { schema, relation }`) to `Completions::requests`. The editor asks the catalog to load those, and repeats the request when the data arrives.

## Dialects

Keywords, built-in functions with a signature detail, and the snippets `sel`, `ins`, `upd`, `del`, `cte` are static tables per dialect. Identifier matching and quoting follow the dialect (see ADR-0023); inserted names go through `sqler_sql::quote_identifier` only when quoting is needed, and a name it rejects drops the candidate instead of being inserted raw. Names written in the SQL itself are inserted the way the dialect resolves them: on `PostgreSQL` `FROM USERS U` offers `u`, and a quoted `"Recent"` stays quoted; `MySQL` and `SQLite` keep them as written.

## Limits

- The context detection is token based and heuristic; odd syntax lowers quality, never causes an error.
- Statements split at every `;` outside strings and comments, so procedure bodies are not special-cased.
- No user-defined functions from the schema yet.
- Hover and diagnostics are specified in ADR-0023 and implemented by T7.
