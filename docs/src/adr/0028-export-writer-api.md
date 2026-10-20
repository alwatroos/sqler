# 0028 Export writer API: push-style row writers, a format registry and SQL literals

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

`sqler-export` was a stub. The results grid (copy-as, T8), the export UI (T9), the CLI (T4) and later the `Parquet` and `XLSX` writers (T3) all need to turn columns and rows into a file or a string. Results can have millions of rows, so a writer must not collect them. AGENTS.md §8 asks for an ADR before a new public API, and this change also makes the `sqler-sql` lexer public and adds SQL literal rendering to `sqler-sql`.

## Decision drivers

- Memory must not depend on the number of rows (AC5).
- One shape for every format, so T3 can add formats without touching callers.
- Every long-running operation is cancellable and reports progress (AGENTS.md §2).
- A generated `INSERT` is data the user runs: no value may end its literal (AC4).
- Few dependencies; `cargo deny` stays clean.

## Considered options

1. A push-style `RowWriter` (`begin`, `write_batch`, `finish`) on any `io::Write`, a `Format` factory, a `Registry`, and an `Exporter` that owns the shared work.
2. A function per format that takes an iterator of rows.
3. A pull-style trait that asks the caller for the next batch.

## Decision outcome

Chosen: option 1.

**Writer API.** `RowWriter::begin(columns)`, `write_batch(&[Row])` (any number of times), `finish(self: Box<Self>)`. `finish` writes the trailer and flushes the sink. A `Format` has an `id`, a `label`, an `extension` and `create(options, sink)`. The sink is `Box<dyn io::Write + Send>`, so a file, a `Vec<u8>`, a pipe or a counting writer all work. The writers are pure renderers and keep no rows. Rejected: option 2 forces an iterator that borrows the source and blocks a push-style driver stream; option 3 puts flow control in the writer and makes cancel harder.

**`Exporter`.** One wrapper handles what every format needs: it returns `State` for calls out of order (a batch or `finish` before `begin`, a second `begin`), checks that every row has as many values as there are columns (`RowWidth`; a bad batch writes nothing), wraps the sink in a 64 KiB buffer and counts the bytes that reach it, splits a batch into chunks of 1,024 rows and checks the `CancelToken` before each chunk and in `finish` (`Cancelled`, no trailer), and calls the `progress` closure after each chunk (the same granularity as cancel) and once in `finish` with `Progress { rows, bytes }`. `Summary` has the totals. `export_to_vec` renders to memory for copy-as and tests.

**Registry.** `Registry` maps ids to formats in registration order: `with_builtin()` has `csv`, `json`, `sql`, `markdown`; `register` rejects a duplicate id. T3 adds `Parquet` and `XLSX` as `#[cfg(feature)]` lines in `with_builtin` and new fields on the `#[non_exhaustive]` `ExportOptions`. Ids are the constants `CSV`, `JSON`, `SQL`, `MARKDOWN`.

**Value rendering.** Every `Value` variant has a defined rendering (the table is in the [architecture page](../architecture/export.md)).

- `CSV` and Markdown share a plain text: `NULL` is the format's `null_text`; booleans `true`/`false`; finite floats use `Display`; `NaN`, `Infinity`, `-Infinity`; bytes are `0x` plus lowercase hex; `JSON` is its raw text; everything else uses `Display`, which is what the grid shows.
- `JSON`: `null`, booleans and integers as literals, finite floats as numbers, `NaN` and the infinities as the strings `"NaN"`, `"Infinity"`, `"-Infinity"` (JSON has no such numbers). Decimals, dates, times and bytes are strings (a decimal keeps its scale). A `JSON` column is embedded as JSON when `serde_json` accepts the text, otherwise it is a string. Duplicate column names get `_2`, `_3`, ... appended.
- `SQL`: the literal table below.

**`CSV`.** RFC 4180: a field is quoted when it holds the delimiter, `"`, CR or LF, or when a non-`NULL` value renders as `null_text`, so `NULL` and empty text stay distinct. Options: delimiter, header, `null_text`, line ending (CRLF default). No `csv` crate: the subset is about 40 lines and the crate would be one more review in `cargo deny`. An opt-in option `neutralize_formulas` (off by default) prefixes text cells (and header names) that start with `=`, `+`, `-`, `@`, tab or CR with a single quote, so a spreadsheet does not run them as formulas. It is off because it changes the data; numbers are never changed.

**SQL literals** live in `sqler-sql/src/dialect.rs` next to `quote_identifier`, because the DML generator (T10) reuses them. They are pure and never fail. The `'` is always doubled.

| Value | `PostgreSQL` | `MySQL` | `SQLite` |
| ----- | ------------ | ------- | -------- |
| Null | `NULL` | `NULL` | `NULL` |
| Bool | `TRUE`/`FALSE` | `TRUE`/`FALSE` | `1`/`0` |
| Int, Decimal | digits | digits | digits |
| Float, finite | `{:?}`; `-0.0` is `'-0'::float8` | `{:?}`; `-0.0` is `-0.0e0` | `{:?}`; `-0.0` is `-0.0e0` |
| NaN, +inf, -inf | `'NaN'::float8`, `'Infinity'::float8`, `'-Infinity'::float8` | `NULL` | `NULL`, `9e999`, `-9e999` |
| Text, Json | `'..'`; with `\` or NUL `E'..'` (`\` to `\\`, NUL to `\x00`) | `'..'`; with `\` or NUL `_utf8mb4 X'<hex>'` | `'..'`; with NUL `CAST(X'<hex>' AS TEXT)` |
| Bytes | `decode('<hex>', 'hex')` | `X'<hex>'` | `X'<hex>'` |
| Date, Time, Timestamp, TimestampTz | `DATE '..'`, `TIME '..'`, `TIMESTAMP '..'`, `TIMESTAMPTZ '..'` | `'..'` | `'..'` |

`MySQL` text with a backslash or NUL is written as hex with an introducer, which is correct under any `sql_mode` (`NO_BACKSLASH_ESCAPES` changes how `\` is read in a plain string). `PostgreSQL` text with a backslash uses `E''` so `standard_conforming_strings` does not matter. `SQLite` has no backslash escapes, only NUL needs the hex form. Negative zero was checked on real servers (PostgreSQL 18, MySQL 8.4, MariaDB 11.8, SQLite 3.54), reading the sign back by text, `atan2(0, x)` or the `f64` bits. `PostgreSQL` types `-0.0e0` as `numeric`, which has no negative zero, so it needs `'-0'::float8` (`CAST('-0' AS DOUBLE PRECISION)` also works). `MySQL` 8.4 keeps the sign of `-0.0e0` (`-0.0` without the exponent is a decimal and loses it). The tests are `db_postgres_float.rs`, `db_mysql_float.rs` and `sqlite_float_literals.rs`. A `proptest` (`tests/sql_props.rs`) renders `INSERT` statements for arbitrary text, floats (finite, NaN, infinities, negative zero), decimals and temporals, re-lexes them with the `sqler-sql` lexer and checks that every token is terminated, that the token sequence is the expected one and that every literal decodes back to the input.

**Public lexer (amends ADR-0020).** ADR-0020 made `lexer.rs` `pub(crate)`. `tokenize`, `Token` and `TokenKind` are now public, so `sqler-export` can re-lex its own output in the proptest. `Token` and `TokenKind` are `#[non_exhaustive]`, so a new kind or field is not a breaking change. The lexing behaviour is unchanged. Rejected: keep the lexer private and put the `INSERT` builder in `sqler-sql`; that moves export code into L1 and overlaps the DML generator.

**Dependencies.** None new. `sqler-export` uses `sqler-core`, `sqler-sql`, `serde_json` and `thiserror` (all in the workspace) and `insta` and `proptest` for tests.

### Limits

- Negative zero is not stored by `MariaDB` 11.8 (every literal form gives `+0.0`) nor by a `SQLite` column with `REAL` affinity (the literal is a negative zero as an expression, but the column stores `+0.0`; a column without affinity keeps it). The writer still emits the best form, so the sign survives wherever the server can keep it.
- A cancelled or failed export leaves the partial output behind; the caller (T8, T9, the CLI) must delete the file.
- `XLSX` (T3) cannot use a `Seek`-less sink with a plain zip writer: it needs a streaming zip writer with data descriptors, or it buffers the whole workbook, which breaks the memory goal.
- A year outside 0..=9999, `NaN` on `PostgreSQL` and NUL in `PostgreSQL` text are written in the core `Display` form or as `E'\x00'`; the server may reject them. That is a loud failure, never an injection. On `MySQL`, `NaN` and the infinities become `NULL`.
- Markdown cells are not HTML-escaped: `<` and `&` pass through, and a Markdown renderer that allows raw HTML will show it. Only `\`, `|` and line breaks are escaped.
- `CSV` with one column and the default empty `null_text` writes a `NULL` row as a blank line. Python `csv`, pandas and Excel skip blank lines, so those rows are lost on import. The encoding (`NULL` as nothing, `""` as an empty string) is kept; use a non-empty `null_text` for single-column exports. Zero columns are rejected (`InvalidOptions`) by `CSV`, Markdown and `SQL`.
- `CSV` formula neutralisation is opt-in and limited to text cells and the header.
- Progress `bytes` counts what reached the sink; up to 64 KiB can still sit in the buffer.

## Consequences

- Good: one API for every format; streaming by construction; cancel and progress in one place; no new crate.
- Good: the SQL literals are tested against the real lexer, and T10 can reuse them.
- Bad: the `sqler-sql` public API grows (three literal functions, the lexer types); `#[non_exhaustive]` keeps them extensible.
- Bad: the sink is a trait object, so each `write` is a virtual call; the 64 KiB buffer makes that cheap.
