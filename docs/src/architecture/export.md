# Export

`sqler-export` (L2) turns columns and rows into `CSV`, `JSON`, `SQL` `INSERT` statements, a
Markdown table, a `Parquet` file or an `Excel` (`.xlsx`) workbook. It is a pure library: no async, no UI, no I/O beyond the `io::Write` it is given.
The decisions are in [ADR-0028](../adr/0028-export-writer-api.md) (the API) and
[ADR-0029](../adr/0029-parquet-and-xlsx-writers.md) (`Parquet` and `XLSX`). The desktop app's
**Export Results...** is built on it in `sqler_ui::export` ([ADR-0032](../adr/0032-grid-export.md),
see "Export from the grid" below).

## Shape

```text
caller --> Exporter --> RowWriter (csv | json | sql | markdown | parquet | xlsx) --> BufWriter(64 KiB) --> CountingWrite --> Sink
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
| `Parquet` (feature `parquet`) | `parquet` | `parquet` | `parquet.row_group_rows` (65,536), `parquet.compression` (`Snappy`, or `Uncompressed`) |
| `Excel` (feature `xlsx`) | `xlsx` | `xlsx` | none |

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
whatever the sink received) behind: the caller (the export UI, the CLI, copy-as) must delete it;
for `Parquet` and `XLSX` the partial file is unreadable (no footer, no central directory). Limits: a year outside 0..=9999, `NaN` and NUL on
`PostgreSQL` may be rejected by the server (a loud failure, never an injection); on `MySQL`, `NaN`
and infinity become `NULL`; negative zero is stored as `+0.0` by `MariaDB` and by a `SQLite` `REAL` column; Markdown cells are not HTML-escaped.

## Parquet

Feature `parquet` (on in the crate default; `sqler-app` forwards it as `export-parquet`, which
is in the app default since [ADR-0032](../adr/0032-grid-export.md)). It uses the low-level `SerializedFileWriter` of the `parquet` crate without the
`arrow` feature, over the `Seek`-less sink. The writer buffers one typed vector per column and
writes a row group every `row_group_rows` rows (default 65,536) or every 32 MiB of estimated buffer,
so memory is one row group. `finish` writes the last group and the footer. Compression is Snappy by
default or none. Every field is `OPTIONAL`; names go through the same de-duplication as `JSON`.

| `DataType` | Parquet type | Accepted `Value` |
| ---------- | ------------ | ---------------- |
| Boolean | `BOOLEAN` | Bool |
| Integer | `INT64` | Int |
| Float | `DOUBLE` | Float, Int |
| Date | `INT32` / `DATE` | Date |
| Time | `INT64` / `TIME(NANOS)` | Time |
| Timestamp | `INT64` / `TIMESTAMP(MICROS)`, not adjusted to UTC | Timestamp |
| TimestampTz | `INT64` / `TIMESTAMP(MICROS)`, adjusted to UTC | TimestampTz |
| Binary | `BYTE_ARRAY` | Bytes, Text |
| Decimal, Text, Json, Other | `BYTE_ARRAY` / `STRING` | any (a decimal keeps its exact text and scale) |

`Null` is a null in every column. A value that does not fit its column (the text `infinity` in a
`Timestamp` column, a date whose day count overflows `i32`) fails with `ExportError::Unsupported`
naming the column; it is never written as a null. Timestamps are microseconds (digits below are
dropped); a time zone is converted to the UTC instant and the offset is lost. Zero columns are
`InvalidOptions`; zero rows are a valid file with the schema.

Progress: `Progress.bytes` moves once per row group, not per chunk of 1,024 rows. The file is
written only when a row group is full, so `bytes` stays flat for up to 65,536 rows at a time (and for
the rest of the 32 MiB estimate with large cells); `rows` still advances per chunk. A progress bar
that shows bytes will step, not slide.

## XLSX

Feature `xlsx` (on in the crate default; `sqler-app` forwards it as `export-xlsx`, which is in the
app default). One sheet, `Sheet1`, written as a streaming zip (deflate entries with data
descriptors, so no `Seek`) with our own `SpreadsheetML`; the sheet XML is deflated into the sink as
rows arrive, so memory is one 64 KiB chunk. Row 1 is the header in bold.

| `Value` | Cell |
| ------- | ---- |
| Null | no cell |
| Bool | boolean |
| Int | number up to 2^53 in magnitude, else text with the digits |
| Float | number; `NaN`, `Infinity`, `-Infinity` as text |
| Decimal | number up to 15 significant digits, else exact text |
| Date | date serial (`yyyy-mm-dd`) for 1900-03-01 to 9999-12-31, else ISO text |
| Timestamp, TimestampTz | date-time serial (`yyyy-mm-dd hh:mm:ss`, a zone is converted to UTC); same range rule |
| Time | day fraction (`hh:mm:ss`) |
| Text, Json, Bytes (`0x..`), other | text |

Formulas are never written: text is always an inline string and the writer has no `<f>` path. Text
that starts with `=`, `+`, `-`, `@`, tab or CR also carries the `quotePrefix` cell format, so Excel
keeps it as text. Limits give `ExportError::Limit` with a message that suggests `CSV` or
`Parquet`: more than 16,384 columns, more than 1,048,575 data rows (Excel's 1,048,576 minus the
header), a text cell or header over 32,767 UTF-16 units (so also a binary value over 16,383 bytes: its `0x..` hex text passes 32,767), an entry or archive over 4 GiB (no ZIP64). Sizes and offsets stop at `u32::MAX - 1`, because `0xFFFFFFFF` is the ZIP64 marker.
The time of day is cut down to whole milliseconds, never rounded up, so `23:59:59.9999` stays on its own day and `9999-12-31 23:59:59.999999` stays inside Excel's range. After the first error a writer refuses further `write_batch` and `finish` calls with `ExportError::State`, so a damaged file is never closed as a valid one.

The cell type follows the value, the `XLSX` writer ignores the declared column type. `Parquet` follows the column type: an `Int` in a `Text`, `Decimal` or `Json` column is stringified (those columns take any value), an `Int` in a `Float` column is widened, any other mismatch is `Unsupported`. This is deliberate: a source may report a `Decimal` column whose values arrive as integers, and rejecting those would fail a correct export.
Characters XML cannot hold become `_xHHHH_`. The tests read the files back with `calamine`; Excel
itself is a maintainer check.

## Registry and features

`Registry::with_builtin()` lists `csv`, `json`, `sql`, `markdown`, then `parquet` and `xlsx` when
their features are on. `register` adds a format and rejects a duplicate id. The workspace
dependency is `default-features = false`, so a crate gets only the formats it asks for. The types
(`ParquetOptions`, `ParquetCompression`, the ids `PARQUET` and `XLSX`, the error variants `Limit`,
`Unsupported` and `Encode`) are always compiled; `ParquetFormat` and `XlsxFormat` need their feature.

## Streaming and speed

`crates/sqler-export/tests/streaming.rs` writes 1,000,000 rows of five columns in batches of 1,000
through every registered format into a counting sink, from one reused `Vec<Row>`, and checks that the output
grows while rows are written. `cargo bench -p sqler-export --bench export_throughput [-- --rows N
--batch N]` prints `<id>_rows_per_sec=` and `<id>_mib_per_sec=`. On the development machine (macOS,
release, 1,000,000 rows, row generation included): `csv` 4.1 M rows/s, `json` 5.7 M, `sql` 4.1 M,
`markdown` 4.2 M; `parquet` 4.4 M and `xlsx` 0.6 M (macOS, 1,000,000 rows, ADR-0029 and
[benchmarks](../benchmarks.md) have Linux, peak RSS and binary sizes). `--format ID` runs one format.
`tests/streaming_memory.rs` runs `Parquet` and `XLSX` over 1,000,000 rows in its own process and
fails if the resident set grows by more than 12 MiB between batch 100 and batch 1,000, or if the sink
does not grow within 66 batches (`Parquet`) or 10 (`XLSX`). The bench is not in `bench.yml`.

## Export from the grid

`sqler_ui::export` (no GPUI except `export::view`) connects a result grid to the writers above. One export is one **producer** and one **writer** joined by a bounded channel.

```text
producer --> Feed (tokio mpsc, FEED_CAPACITY = 4 batches) --> write_feed --> Exporter --> .<name>.<pid>-<job>.partial --rename--> target
  loaded rows: LoadedCursor (UI thread, time-boxed slices)        blocking pool (SessionBridge::blocking)
  fresh run:   fresh_rows (own session through the Connector)
```

- **Feed** is `Columns`, `Rows`, `Done` or `Failed(ExportFailure)`. The channel holds at most four batches, so a slow disk stops the producer (backpressure) and memory does not depend on the row count. A test pushes 1,000,000 rows and checks the rows in flight.
- **Writer.** `write_feed` runs on tokio's blocking pool, never on the GPUI thread. It creates a `PartialFile` (a hidden sibling, `create_new`), drives the `Exporter`, and on success fsyncs and renames it over the target. A symlink target is replaced by a regular file; on Unix a replaced regular file keeps its permissions (best effort). Any failure or cancel drops the partial file. The protocol is the one of `sqler-cli export`, re-implemented because that type is private.
- **Loaded rows.** The `GridModel` belongs to the UI thread, so `LoadedCursor` copies rows from it in slices of a few milliseconds and feeds them to the channel. A selection copies the selected rectangle in display order (`LoadedRows::Picked`).
- **Fresh run.** `fresh_rows` opens a short-lived session through the `Connector` (as the object browser does for table data), runs the statement and streams the batches with `pump_stream`. It is allowed only when `sqler_sql::classify_each` gives exactly one `Read` statement the class recorded when the tab ran it is also `Read`, and the connection has a dialect (`fresh_gate`), and `Workspace::fresh_check` then checks that the tab still has its connection; otherwise the dialog says why and offers only the loaded rows. Nothing but a statement classified as a read is run a second time. The gate is lexical (no write syntax found), and behind it the fresh session is made read-only before the statement runs (`PRAGMA query_only` on `SQLite`, a read-only session characteristic on PostgreSQL and MySQL), so the database refuses a write the classifier missed. `DuckDB` and SQL Server have no session setting for it: there only the lexical gate applies, and a view, user-defined operator or cast that writes is a known blind spot.
- **Cancel.** `ExportStop` wraps a `CancelToken` and a watch channel; it stops the producer, the connect and the writer. Closing the tab or quitting cancels its export.
- **State.** The dialog, the form, the running job and the progress live in `Workspace` (`workspace/export.rs`, headless, tested without a window). It emits two effects, `PickExportPath` (the native save dialog) and `RunExport`; `workspace/view/export.rs` and `export/view.rs` carry them out and draw the dialog and the progress bar.
- **Errors.** `ExportFailure` carries a user text per kind. `Connect` and `Driver` hold the text the connect pipeline or the driver shows the user (`connect_text`, `driver_text`: built from `Display` only, with the `Tunnel`, `Target`, `Secret`, `invalid ... target` and `Backend` variants mapped to fixed texts); no variant carries the SQL, the path or a credential. The database's own message can quote a token of the statement, as in the query tab.

`cargo bench -p sqler-ui --bench grid_export` measures both producers; figures are on the [Benchmarks](../benchmarks.md) page.

## Tests

Unit tests sit next to each writer. `tests/snapshots.rs` has one `insta` snapshot per format over a
fixture with every `Value` variant and the awkward cases. `tests/sql_props.rs` renders `INSERT`
statements for arbitrary text and re-lexes them with the `sqler-sql` lexer. `tests/streaming.rs` is
the 1M-row test over every registered format. `tests/parquet_roundtrip.rs` reads files back with
the `parquet` reader and `tests/xlsx_readback.rs` with `calamine`.
