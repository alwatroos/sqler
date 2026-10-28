# SQL text services

`sqler-sql` (L1) holds the pure text services the editor (T9), the query runner (T8) and the AI
guard build on. Nothing here does I/O or async. The decision and its trade-offs are in
[ADR-0020](../adr/0020-sql-text-services.md); DDL generation is in
[ADR-0015](../adr/0015-ddl-generation-dialects-and-quoting.md).

## One lexer, five services

```text
text --> lexer (per dialect, never fails, tokens tile the input)
            |--> split        Segment { kind, span, body, terminator }
            |--> statement_at the segment under a cursor offset
            |--> placeholders $n, ?, ?n, :name, @name, $name with ranges
            |--> classify     Read < Other < Write < Ddl < Unknown
            `--> format       token-preserving layout, idempotent
```

The lexer is public (`tokenize`, `Token` and `TokenKind`; the two types are `#[non_exhaustive]`; ADR-0028 amends
ADR-0020) so callers can re-lex generated SQL. Unterminated strings, quoted identifiers and comments run to the end of
the input and carry a flag, so a half-typed script still lexes. All functions are a single pass
over the tokens: linear time, no recursion.

## Splitting and the cursor

`split(dialect, text)` returns segments whose spans tile the whole text. A segment is a
`Statement`, a `Delimiter` (a `MySQL` `DELIMITER x` line) or `Trivia` (only whitespace and
comments). A `;` ends a statement only outside strings, quoted identifiers, comments, dollar
quotes, `BEGIN ... END` blocks of triggers and routines, and (on `PostgreSQL`) parentheses.
`split_with(dialect, text, SplitOptions { blank_lines: true })` also ends a statement at a top-level
blank line (two or more line breaks) with no terminator. It does so only when the piece has content,
sits outside parentheses, blocks and routine bodies, no `MySQL` custom delimiter is active, the next
token that is not a comment is a statement keyword (`SELECT`, `WITH`, `INSERT`, ...), the piece does
not end with an operator, a separator or a continuation word (`UNION`, `AS`, `FROM`, ...) and is not a
`WITH` clause ending in `)`. Comments after the blank line belong to the next segment. `split` is
`split_with` with the default options, so classification, transactions, intellisense and the AI write
guard keep their boundaries; only the editor run path (`Workspace::run_editor_query`, editor explain)
opts in. Each piece is classified on its own, so `select 1`, a blank line, then `delete from t` gives a
`Read` and a `Write`; nothing is merged, so the fail-safe classification is not weakened.

`statement_at(&segments, offset)` returns the statement containing the offset, else the nearest
preceding one, else the first.

## Placeholders

`placeholders(dialect, text)` lists the parameters the dialect has: `PostgreSQL` `$n` and `:name`,
`MySQL` `?` and `:name`, `SQLite` `?`, `?n`, `:name`, `@name` and `$name`. Text in strings,
comments and dollar bodies, `PostgreSQL` array slices (`a[1:n]`) and compound statements give none.

## Classification

`classify` returns the most severe class over the statements; `classify_each` returns one per
statement. `Read` needs a read lead word and a clean whole-statement scan (no write or DDL word, no
executable comment, no non-allowlisted function call). Anything else is `Other`, `Write`, `Ddl` or
`Unknown`, and only `Read` is read-only. `Read` means "no write syntax found": a view, a
user-defined operator or a cast can still write, so a guard that must be certain also runs the
statement in a read-only transaction.

## Formatting

`format(dialect, text, &FormatOptions)` lays out the tokens (keyword case, line breaks before
clauses and joins, subquery indentation) without adding, removing or reordering any, so comments
survive and formatting twice gives the same text. Input with an unterminated token gives
`FormatError::Unterminated`.

## SQL literals

`sql_literal(dialect, &Value)`, `push_literal` and `string_literal` render a `sqler-core` value as a
SQL literal. They are pure, never fail and never leave a string open: `'` is doubled, and text with
a backslash or NUL uses `E'..'` (`PostgreSQL`), a hex string with `_utf8mb4` (`MySQL`) or a hex
`CAST` (`SQLite`, NUL only). `NaN` and the infinities have a form per dialect. The table is in
[ADR-0028](../adr/0028-export-writer-api.md); `sqler-export` uses them for `INSERT` files, and
its `proptest` re-lexes the output with the public lexer.

## DML generation

`dml` builds the statements for staged grid edits (`UPDATE`, `INSERT`, `DELETE`) as `Query` values:
identifiers are quoted, the dialect's native positional markers are written directly (`$k`,
`?k`, `?`) and every value travels in `Query::params`, so no value text reaches the SQL. An empty
filter is an error, so an unfiltered write cannot be generated. `PostgreSQL` text-bound kinds get a
`CAST` to the column type without its length or precision, so the server's assignment check still rejects a value that does not fit. `single_table_source` accepts only a plain `SELECT` of `*` or plain
columns from one table, which is how a query result is proven to come from a single table. The
editability rules, the apply transaction and the row-count checks are in
[ADR-0030](../adr/0030-staged-edits-and-dml.md).

## Tests

Unit tests sit next to the lexer, the splitter and the placeholder scanner. Case tables per
dialect and `proptest` properties (spans tile the input, nothing panics, a write keyword never
classifies as read, formatting is idempotent) are in `crates/sqler-sql/tests/`, together with
MiB-sized hostile inputs.
DML generation has per-dialect exact-SQL unit tests and `proptest` properties that no value text appears in the SQL and that placeholders match `params`.
