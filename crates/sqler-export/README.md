# sqler-export

Result writers for `CSV`, `JSON`, `SQL` `INSERT`, Markdown tables, `Parquet` and `XLSX`.

Status: M4: push-style row writers on any `io::Write`, with cancel and progress

Layer: L2

API (pure, no async, no I/O beyond the sink):

- `RowWriter` (`begin(columns)`, `write_batch(rows)`, `finish`), `Format` (`id`, `label`, `extension`, `create`), `Registry` (`with_builtin`, `register`, `get`, `formats`).
- `Exporter` drives a writer: call order, row width, a 64 KiB buffer, byte counting, `CancelToken` and `Progress`; it returns a `Summary`. `export_to_vec` renders to memory.
- Formats `CsvFormat`, `JsonFormat`, `SqlFormat`, `MarkdownFormat` (ids `CSV`, `JSON`, `SQL`, `MARKDOWN`) and `ExportOptions` (`csv`, `sql`, `markdown`, `parquet`).
- `ParquetFormat` (feature `parquet`, id `PARQUET`; row groups of 65,536 rows, Snappy) and `XlsxFormat` (feature `xlsx`, id `XLSX`; one sheet, typed cells, never a formula). Both are on by default in this crate; a dependent that sets `default-features = false` gets only the formats it names. Excel limits (rows, columns, cell length) give `ExportError::Limit`. For `Parquet`, `Progress.bytes` moves once per row group.
- `CSV` is RFC 4180 with a configurable delimiter, header, `NULL` text and line ending; `neutralize_formulas` (off by default) prefixes text cells that start with `=`, `+`, `-`, `@`, tab or CR with a single quote.

```rust
use sqler_core::{Column, DataType, Row, Value};
use sqler_export::{CsvFormat, ExportOptions, export_to_vec};

let columns = [Column::new("id", DataType::Integer)];
let rows = [Row(vec![Value::Int(1)])];
let csv = export_to_vec(&CsvFormat, &ExportOptions::default(), &columns, &rows)?;
```

Memory does not depend on the row count (`tests/streaming_memory.rs` checks 1M rows for `Parquet` and `XLSX`). Benchmark: `cargo bench -p sqler-export --bench export_throughput`.
See [ADR-0028](../../docs/src/adr/0028-export-writer-api.md), [ADR-0029](../../docs/src/adr/0029-parquet-and-xlsx-writers.md) and the
[architecture page](../../docs/src/architecture/export.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
