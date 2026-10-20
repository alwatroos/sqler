# Export

`sqler-export` (L2) turns columns and rows into `CSV`, `JSON`, `SQL` `INSERT` statements or a
Markdown table. It is a pure library: no async, no UI, no I/O beyond the `io::Write` it is given.
The decisions are in [ADR-0028](../adr/0028-export-writer-api.md).

## Shape

```text
caller --> Exporter --> RowWriter (csv | json | sql | markdown) --> BufWriter(64 KiB) --> CountingWrite --> Sink
            |  order, row width, chunks of 1,024 rows, cancel, progress
Registry: id --> Format --> create(options, sink)
```

A `RowWriter` has `begin(columns)`, `write_batch(rows)` and `finish`. It keeps no rows, so memory is
the same for ten rows and for a million. The `Exporter` checks the call order (`State`), the width of
every row (`RowWidth`) and the `CancelToken` before every chunk of 1,024 rows and in `finish`
(`Cancelled`, no trailer). It reports `Progress { rows, bytes }` after each chunk (so a large batch reports as it goes) and
once in `finish`; `bytes` is what reached the sink. `export_to_vec` is the one-call form for copy-as.

```rust,ignore
use sqler_export::{ExportControl, ExportOptions, Exporter, Registry};

let registry = Registry::with_builtin();
let format = registry.get("csv").unwrap();
let mut exporter = Exporter::new(format, &ExportOptions::default(), Box::new(file), ExportControl::default())?;
exporter.begin(&columns)?;
for batch in batches { exporter.write_batch(&batch)?; }
let summary = exporter.finish()?; // rows and bytes
```

## Formats and options

| Format | Id | Extension | Options |
| ------ | -- | --------- | ------- |
| `CSV` | `csv` | `csv` | `delimiter` (ASCII, default `,`), `header` (true), `null_text` (empty), `line_ending` (CRLF), `neutralize_formulas` (false) |
| `JSON` | `json` | `json` | none |
| `SQL` | `sql` | `sql` | `dialect` (`PostgreSQL`), `schema`, `table` (required), `rows_per_statement` (100) |
| Markdown | `markdown` | `md` | `null_text` (`NULL`) |

`CSV` follows RFC 4180. A field is quoted when it holds the delimiter, `"`, CR or LF. A value that
is not `NULL` but renders as `null_text` is quoted too, so with the default empty `null_text` an
empty string is written as `""` and `NULL` as nothing. The delimiter must be ASCII and not `"`, CR
or LF, and `null_text` must not contain the delimiter, `"`, CR or LF (`InvalidOptions`).
With a single column and the default empty `null_text`, a `NULL` row is a blank line, which
Python `csv`, pandas and Excel skip on import; set a non-empty `null_text` for single-column
exports. An export with zero columns is rejected (`InvalidOptions`) by `CSV`, Markdown and `SQL`.

`neutralize_formulas` is for files that will be opened in a spreadsheet. When it is on, a text or
`JSON` cell (and a header name) that starts with `=`, `+`, `-`, `@`, tab or CR gets a leading `'`,
so `=1+1` is written as `'=1+1`. Numbers are never changed. It is off by default because it
changes the data.

`JSON` is `[`, one object per line separated by `,` and a closing `]`; no rows give `[]`. Keys are
the column names, and a repeated name gets `_2`, `_3`, ... appended.

`SQL` writes `INSERT INTO <schema>.<table> (<columns>) VALUES` followed by one tuple per row;
a new statement starts after `rows_per_statement` rows. Identifiers go through `quote_identifier`,
values through `sql_literal`. No rows write nothing.

Markdown writes a header row, a `| --- |` row and one line per row. `\` and `|` are escaped and line
breaks become `<br>`. There is no width padding, because output is streamed.

## Value rendering

| Value | `CSV` and Markdown | `JSON` | `SQL` |
| ----- | ------------------ | ------ | ----- |
| Null | `null_text` | `null` | `NULL` |
| Bool | `true`, `false` | `true`, `false` | per dialect (`TRUE`, `1`) |
| Int | digits | number | digits |
| Float | `Display`; `NaN`, `Infinity`, `-Infinity` | number; the strings `"NaN"`, `"Infinity"`, `"-Infinity"` | `{:?}`; negative zero is `'-0'::float8` (`PostgreSQL`) or `-0.0e0`; per dialect for NaN and infinity |
| Text | as is | string | string literal per dialect |
| Bytes | `0x` + lowercase hex | string `"0x.."` | `decode(.., 'hex')` or `X'..'` |
| Decimal | as is | string (keeps the scale) | digits |
| Date, Time, Timestamp, TimestampTz | `Display` | string | `DATE '..'` etc. on `PostgreSQL`, `'..'` elsewhere |
| Json | raw text | embedded when valid, else a string | string literal |

The full literal table is in the ADR. A cancelled or failed export leaves the partial file (or
whatever the sink received) behind: the caller (the export UI, the CLI, copy-as) must delete it.
`XLSX` (T3) needs a streaming zip writer with data descriptors, since writers get no `Seek`; the
alternative, buffering the whole workbook, breaks the memory goal. Limits: a year outside 0..=9999, `NaN` and NUL on
`PostgreSQL` may be rejected by the server (a loud failure, never an injection); on `MySQL`, `NaN`
and infinity become `NULL`; negative zero is stored as `+0.0` by `MariaDB` and by a `SQLite` `REAL` column; Markdown cells are not HTML-escaped.

## Registry and later formats

`Registry::with_builtin()` lists the four formats in the order above. `register` adds a format and
rejects a duplicate id. `Parquet` and `XLSX` (T3) are added behind features with a line in
`with_builtin` and new fields in `ExportOptions` (`#[non_exhaustive]`).

## Streaming and speed

`crates/sqler-export/tests/streaming.rs` writes 1,000,000 rows of five columns in batches of 1,000
through every format into a counting sink, from one reused `Vec<Row>`, and checks that the output
grows while rows are written. `cargo bench -p sqler-export --bench export_throughput [-- --rows N
--batch N]` prints `<id>_rows_per_sec=` and `<id>_mib_per_sec=`. On the development machine (macOS,
release, 1,000,000 rows, row generation included): `csv` 4.1 M rows/s, `json` 5.7 M, `sql` 4.1 M,
`markdown` 4.2 M. The bench is not in `bench.yml`.

## Tests

Unit tests sit next to each writer. `tests/snapshots.rs` has one `insta` snapshot per format over a
fixture with every `Value` variant and the awkward cases. `tests/sql_props.rs` renders `INSERT`
statements for arbitrary text and re-lexes them with the `sqler-sql` lexer. `tests/streaming.rs` is
the 1M-row test.
