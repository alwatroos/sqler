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

The lexer is crate-private. Unterminated strings, quoted identifiers and comments run to the end of
the input and carry a flag, so a half-typed script still lexes. All functions are a single pass
over the tokens: linear time, no recursion.

## Splitting and the cursor

`split(dialect, text)` returns segments whose spans tile the whole text. A segment is a
`Statement`, a `Delimiter` (a `MySQL` `DELIMITER x` line) or `Trivia` (only whitespace and
comments). A `;` ends a statement only outside strings, quoted identifiers, comments, dollar
quotes, `BEGIN ... END` blocks of triggers and routines, and (on `PostgreSQL`) parentheses.
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

## Tests

Unit tests sit next to the lexer, the splitter and the placeholder scanner. Case tables per
dialect and `proptest` properties (spans tile the input, nothing panics, a write keyword never
classifies as read, formatting is idempotent) are in `crates/sqler-sql/tests/`, together with
MiB-sized hostile inputs.
