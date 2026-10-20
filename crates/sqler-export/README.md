# sqler-export

Result writers for `CSV`, `JSON`, `SQL` `INSERT` and Markdown tables. `Parquet` and `XLSX` follow (T3).

Status: M4: push-style row writers on any `io::Write`, with cancel and progress

Layer: L2

API (pure, no async, no I/O beyond the sink):

- `RowWriter` (`begin(columns)`, `write_batch(rows)`, `finish`), `Format` (`id`, `label`, `extension`, `create`), `Registry` (`with_builtin`, `register`, `get`, `formats`).
- `Exporter` drives a writer: call order, row width, a 64 KiB buffer, byte counting, `CancelToken` and `Progress`; it returns a `Summary`. `export_to_vec` renders to memory.
- Formats `CsvFormat`, `JsonFormat`, `SqlFormat`, `MarkdownFormat` (ids `CSV`, `JSON`, `SQL`, `MARKDOWN`) and `ExportOptions` (`csv`, `sql`, `markdown`).
- `CSV` is RFC 4180 with a configurable delimiter, header, `NULL` text and line ending; `neutralize_formulas` (off by default) prefixes text cells that start with `=`, `+`, `-`, `@`, tab or CR with a single quote.

```rust
use sqler_core::{Column, DataType, Row, Value};
use sqler_export::{CsvFormat, ExportOptions, export_to_vec};

let columns = [Column::new("id", DataType::Integer)];
let rows = [Row(vec![Value::Int(1)])];
let csv = export_to_vec(&CsvFormat, &ExportOptions::default(), &columns, &rows)?;
```

Memory does not depend on the row count. Benchmark: `cargo bench -p sqler-export --bench export_throughput`.
See [ADR-0028](../../docs/src/adr/0028-export-writer-api.md) and the
[architecture page](../../docs/src/architecture/export.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
