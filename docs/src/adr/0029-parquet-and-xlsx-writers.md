# 0029 Parquet and XLSX writers: own streaming zip, low-level Parquet API, two cargo features

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the binary size budget is now 70 MB (73,400,320 bytes); the 60 MB figures below are the budget at the time of writing.
- Date: 2026-10-21

## Context and problem statement

T3 adds `Parquet` and `Excel` (`.xlsx`) to `sqler-export` behind the writer API of [ADR-0028](0028-export-writer-api.md) (`RowWriter`, `Format`, `Registry`; no change to them). Results can have millions of rows, so memory must not depend on the row count (AC4), the sink has no `Seek`, the release binary must stay under 60 MB (prompt §2.2), every dependency passes `cargo deny` with no advisory ignore and no `rsa` crate (owner decision 1), and a text cell that starts with `=`, `+`, `-` or `@` must never become a formula.

## Decision drivers

- Streaming into a `Write`-only sink, memory bounded by one row group or one chunk.
- Binary size and compile time (the Linux app binary is already over budget before T14).
- No new advisory, no new licence, few new crates.
- Formulas can not be written, by construction.

## Considered options

`Parquet`:
1. `parquet` crate, low-level `SerializedFileWriter`, `default-features = false`, feature `snap` only.
2. `parquet` with `ArrowWriter`: less code, but adds about seven `arrow-*` crates (`arrow-array`, `arrow-buffer`, `arrow-data`, `arrow-schema`, `arrow-select`, ...) and a column model that duplicates `Value`.

`XLSX`:
1. A small writer of our own: one-sheet `SpreadsheetML` package in a streaming zip (deflate entries with data descriptors) over `flate2`.
2. `rust_xlsxwriter`: `save_to_writer` needs `Write + Seek`, and its constant-memory mode spools to temporary files and zips at save, so it does not stream into the sink.
3. The `zip` crate as the container: a dependency with heavy default features (bzip2, zstd, aes, ...) for about 150 lines of code.

## Decision outcome

Chosen: Parquet option 1, XLSX option 1.

**Crates** (latest stable verified on the crates.io API on 2026-10-21; rows in [dependencies](../dependencies.md)).

| Crate | Version | Released | Licence | `rust-version` | Used for |
| ----- | ------- | -------- | ------- | -------------- | -------- |
| `parquet` | 60.0.0 | 2026-10-15 | `Apache-2.0` | 1.88 | Parquet writer; `default-features = false`, feature `snap` |
| `snap` | 1.1.2 | 2026-07-15 | `BSD-3-Clause` | none | Snappy, pulled by `parquet/snap` |
| `flate2` | 1.1.10 | 2026-08-28 | `MIT OR Apache-2.0` | 1.67 | deflate and CRC-32 of the zip; pure-Rust `miniz_oxide` backend, already in `Cargo.lock` |
| `calamine` | 0.37.0 | 2026-10-18 | `MIT` | 1.88 | dev only: reads `.xlsx` back in tests |
| `memory-stats` | 1.2.0 | 2024-06-26 | `MIT OR Apache-2.0` | none | dev only: RSS in the 1M-row memory test |
| `bytes` | 1.12.1 | 2026-07-08 | `MIT` | 1.57 | dev only: `Bytes` as `ChunkReader` for the Parquet read-back |

`cargo deny check` (advisories, bans, licences, sources) passes with no new ignore and no new licence in `deny.toml`; `cargo tree -i rsa` matches nothing; `parquet` does not pull `paste`, so the existing `paste` ignore is not involved. Extra crates over a build with neither format (`cargo tree -p sqler-export -e normal`): 22 for `parquet` (`chrono`, `iana-time-zone`, `half`, `num-bigint`, `num-integer`, `num-traits`, `ahash`, `hashbrown`, `twox-hash`, `snap`, `bytes`, `zerocopy`, `libm`, `getrandom`, `once_cell`, `libc`, `cfg-if`, and the proc-macro crates `seq-macro`, `zerocopy-derive` with `syn`), 6 for `xlsx` (`flate2`, `miniz_oxide`, `adler2`, `crc32fast`, `simd-adler32`, `cfg-if`), 27 for both. The only `*-sys` crate is `core-foundation-sys`, on macOS only, reached through `parquet` -> `chrono` -> `iana-time-zone` (`chrono` is not optional in `parquet`): a pure-Rust binding with no C build and no bundled library, so no native code is added, but a literal "no `*-sys` crate" check does not hold on macOS. `calamine` pulls `zip`, `zopfli` and `quick-xml` into `Cargo.lock` as dev-dependencies only; they are not in any shipped binary.

**Parquet.** `SerializedFileWriter` over the `Seek`-less sink. The writer buffers per-column typed vectors with definition levels and writes one row group every `row_group_rows` rows (default 65,536, `ParquetOptions`) or every 32 MiB of estimated buffer, whichever comes first, so a table of large cells does not grow without bound. Compression is `Snappy` (default) or `Uncompressed` (`ParquetCompression`); `zstd`, `lz4` and `brotli` are not compiled in. `finish` writes the last group and the footer and flushes the sink. A file with zero rows is valid. Every field is `OPTIONAL`; names come from `unique_names`, as in `JSON`. `created_by` is `sqler <version>`.

| `DataType` | Physical / logical | Accepted `Value` |
| ---------- | ------------------ | ---------------- |
| Boolean | `BOOLEAN` | Bool |
| Integer | `INT64` | Int |
| Float | `DOUBLE` | Float, Int |
| Date | `INT32` / `DATE` | Date (`Unsupported` when the day count overflows `i32`) |
| Time | `INT64` / `TIME(NANOS, utc=false)` | Time |
| Timestamp | `INT64` / `TIMESTAMP(MICROS, utc=false)` | Timestamp |
| TimestampTz | `INT64` / `TIMESTAMP(MICROS, utc=true)`, stored as UTC | TimestampTz |
| Binary | `BYTE_ARRAY` | Bytes, Text (its UTF-8 bytes) |
| Decimal, Text, Json, Other | `BYTE_ARRAY` / `STRING` | any value, rendered as in `CSV` (a decimal keeps its exact text and scale) |

`Null` is a null in every column. Any other pairing (for example the text `infinity` in a `Timestamp` column) is `ExportError::Unsupported { column, reason }` and never a silent null. Zero columns give `InvalidOptions`. Rejected: `DECIMAL(38, s)`, because precision and scale are unknown at `begin` and values can exceed 38 digits. Timestamps are `MICROS` (about +-290,000 years); digits below a microsecond are dropped. A `TimestampTz` keeps the instant; the offset is lost.

**XLSX.** The package is `[Content_Types].xml`, `_rels/.rels`, `xl/workbook.xml` (one sheet, `Sheet1`), `xl/_rels/workbook.xml.rels`, `xl/styles.xml` and `xl/worksheets/sheet1.xml`, written to the sink as a zip with deflate entries, general-purpose flags bit 3 (sizes and CRC in a data descriptor after the data) and bit 11 (UTF-8 names), DOS date 1980-01-01, then the central directory and the end record. The sheet XML is deflated into the sink in chunks of 64 KiB while rows arrive, so memory is one chunk. No ZIP64: a sheet entry or an archive over `u32::MAX` bytes is `ExportError::Limit`.

| `Value` | Cell |
| ------- | ---- |
| Bool | boolean `0`/`1` |
| Int | number when \|n\| <= 2^53, else an `inlineStr` of the digits |
| Float | number; `NaN`, `Infinity`, `-Infinity` as `inlineStr` |
| Decimal | number up to 15 significant digits, else exact `inlineStr` |
| Date | serial number, style `yyyy-mm-dd`, for 1900-03-01 to 9999-12-31; otherwise ISO `inlineStr` |
| Timestamp, TimestampTz (as UTC) | serial plus day fraction, style `yyyy-mm-dd hh:mm:ss`; same range rule |
| Time | day fraction, style `hh:mm:ss` |
| Text, Json, Bytes (`0x..`), other | `inlineStr` |
| Null | no cell |

Row 1 is the header in bold. **Formulas are never written.** The writer has no code path that emits `<f>`, and text is always an `inlineStr`. A text cell (or header) that starts with `=`, `+`, `-`, `@`, tab or CR also gets a cell format with `quotePrefix="1"`, so Excel shows and keeps it as text even when the user edits it. Escaping: `& < > "`; control characters other than tab, LF and CR, and U+FFFE/U+FFFF, become `_xHHHH_`; a literal `_xHHHH_` becomes `_x005F_xHHHH_`.

Limits, each an `ExportError::Limit` whose message suggests `CSV` or `Parquet` (assumed in the brief: an error, not a split): more than 16,384 columns, more than 1,048,575 data rows (the header takes one of Excel's 1,048,576; the check runs before the chunk that would pass it, so a stream of exactly 1,048,575 works), a text cell or header over 32,767 UTF-16 units (so a binary value over 16,383 bytes fails too: its `0x..` hex text passes 32,767), an entry or archive over 4 GiB (sizes and offsets stop at `u32::MAX - 1`; `0xFFFFFFFF` is the ZIP64 marker). The time of day is cut down to whole milliseconds, never rounded up, so a time of 23:59:59.9999 does not become 1.0 (shown as 00:00:00) and `9999-12-31 23:59:59.999999` stays inside Excel's last day. After its first error either writer is locked: `write_batch` and `finish` return `ExportError::State`, so a caller that ignores the error cannot close a damaged or truncated file as valid.

**Cross-type policy.** `Parquet` follows the declared column type; `XLSX` follows the value. An `Int` in a `Text`, `Decimal` or `Json` column is stringified in `Parquet` and a number in `XLSX`. Rejecting it was considered and not chosen: sources report some `Decimal` columns whose values arrive as integers, and `tests/parquet_roundtrip.rs` fixes that text columns take any value. A value that cannot be stored in its typed column (text in a `Timestamp` column) is `Unsupported` in `Parquet`.

**Cargo features.** `sqler-export` has `parquet = ["dep:parquet"]` and `xlsx = ["dep:flate2"]`; both are in the crate's own `default`, so its tests run in CI. The workspace line is `default-features = false`, so a dependent gets only the formats it asks for. The option and error types (`ParquetOptions`, `ParquetCompression`, `Limit`, `Unsupported`, `Encode`) and the ids `PARQUET` and `XLSX` are always compiled; only `ParquetFormat` and `XlsxFormat` and their registry lines depend on a feature. `--no-default-features`, `--features parquet`, `--features xlsx` and the default all build, lint and pass their tests. `sqler-app` forwards `export-xlsx` (in its `default`) and `export-parquet` (opt-in at the time; in the default since [ADR-0032](0032-grid-export.md)). T4 enables both formats in `sqler-cli`.

### Measured cost

All numbers are real, taken on 2026-10-21 with `rustc 1.99.1`, `cargo build --release --locked` with the workspace `[profile.release]` of T14 (`strip = "symbols"`, `codegen-units = 1`, no LTO), re-measured after merging `main` (the first measurement, before T14, had the default profile: symbols kept, 16 codegen units). macOS: Apple M4 Pro, 14 cores, arm64. Linux: Docker image `rust:1.99.1` (Debian, `aarch64` because the host is arm64; no `x86_64` build was run), container `sqler-t3-w4-linux`. `sqler-export` has no GPUI, so the system libraries `sqler-app` needs are not involved.

Binary size: the `export_throughput` bench binary (it links the whole `Registry`, so every enabled format is live code), built with `cargo build --release -p sqler-export --bench export_throughput <features>`. `strip` is `strip` on a copy.

| Features | macOS bytes | Linux aarch64 bytes |
| -------- | ----------- | ------------------- |
| none (`--no-default-features`, four text formats) | 497,488 | 529,240 |
| `xlsx` | 598,544 (+101,056) | 594,776 (+65,536) |
| `parquet` | 1,157,536 (+660,048) | 1,184,624 (+655,384) |
| `xlsx` + `parquet` (default) | 1,241,856 (+744,368) | 1,315,696 (+786,456) |

The binaries are already stripped by the profile, so there is no separate stripped column. So `xlsx` costs about 0.07 to 0.1 MB and `parquet` about 0.66 MB; both together about 0.75 to 0.79 MB. Before T14 (symbols kept, 16 codegen units) the same bench binaries were 632,464 / 784,912 / 1,527,648 / 1,645,360 bytes on macOS and 738,064 / 909,088 / 1,655,392 / 1,823,928 on Linux, which was +0.15 MB for `xlsx` and +0.9 MB for `parquet` unstripped.

Desktop app, `cargo build --release --locked -p sqler-app` on macOS (the base is `HEAD` of the branch before T3, commit `16b816e`):

| Build | Bytes |
| ----- | ----- |
| base, before T3 | 44,362,448 |
| T3, default (`export-xlsx`) | 44,362,448 (+0) |
| T3, `--features export-parquet` | 44,376,272 (+13,824) |

The app delta is about 0 because nothing in `sqler-app` or `sqler-ui` calls `sqler-export` yet, so the linker drops it. T9 links the registry and then the bench-binary figures above are the projected cost (about +0.1 MB for `xlsx`, +0.66 MB more for `parquet`). The Linux app binary was not measured here; it was 79.6 MiB before T3 and T14, with the default release profile (T14 measured 51.1 MiB for the x86_64 build with its profile; see `benchmarks.md`).

Compile time: `cargo build --release -p sqler-export --lib` from an empty `CARGO_TARGET_DIR`, wall clock, macOS: none 2.3 s, `xlsx` 2.6 s, `parquet` 7.0 s, both 7.2 s. The Linux container timings (8.1, 4.6, 15.3 and 6.7 s) include crate downloads and a rustup sync on first use, so they are not comparable and are not used.

Throughput and peak RSS: `export_throughput --rows N --format ID`, one process per format and row count, 5 columns, batches of 1,000, sink that discards, row generation included, `/usr/bin/time -l` (macOS) and `-v` (Linux). Run-to-run noise is about 10 % (two runs of the same macOS `xlsx` binary gave 550 and 604 thousand rows/s).

The rows below come from the build with both formats (default features) unless a column says otherwise; the single-format `xlsx` build peaks at 3.3 MB on macOS (re-measured after the merge of `main`) and 2.8 MB on Linux, the figures in `benchmarks.md`.

| Format | macOS 1M rows/s | macOS MiB/s | macOS peak RSS 100k / 1M | Linux 1M rows/s | Linux peak RSS 100k / 1M |
| ------ | --------------- | ----------- | ------------------------ | --------------- | ------------------------ |
| `parquet` (Snappy) | 4.38 M | 76 | 27.6 MB / 34.1 MB (27.9 MB / 31.8 MB in the build with both formats) | 4.74 M | 15.4 MB / 15.9 MB |
| `xlsx` | 0.60 M | 14.9 | 7.3 MB / 7.3 MB | 0.76 M | 3.1 MB / 3.1 MB |
| `csv` (for comparison) | 3.37 M | 204 | 6.5 MB / 6.5 MB | 3.99 M | 2.6 MB / 2.8 MB |

Memory proof, `tests/streaming_memory.rs` (its own test binary, one `#[test]`, 1,000,000 rows in 1,000 batches, `memory-stats` RSS): resident set after batch 100 vs after batch 1,000, `cargo test --release`:

| | Parquet after 100k rows | Parquet after 1M | `XLSX` after 100k | `XLSX` after 1M |
| - | ----------------------- | ---------------- | ----------------- | --------------- |
| macOS | 21.9 MB | 27.7 MB (+5.8 MB) | 27.9 MB | 27.9 MB (+0) |
| Linux aarch64 | 15.7 MB | 17.9 MB (+2.2 MB) | 13.6 MB | 13.6 MB (+0) |

The test fails above +12 MiB (about twice the worst measured growth, +5.8 MB, and below either output), and also fails if the sink has not grown within 66 batches (Parquet; a row group is 65,536 rows) or 10 batches (`XLSX`; one deflate chunk), which catches a writer that holds its output back until `finish`. The `XLSX` baseline includes the heap left by the Parquet pass that runs first. Limit of the proof: the whole output is 18 MB (Parquet) or 26 MB (`XLSX`), below the RSS threshold, the 12 MiB bound is below both, so a writer that keeps its output also fails the RSS check; the sink check catches one that holds output back until `finish`. Measured in this round: 1.0-1.8 MB for Parquet over ten release runs and 0.9-5.7 MB over ten debug runs, 0 for `XLSX` in both.

### Whether Parquet is in the app default

`export-parquet` is not in the `sqler-app` default. The binary delta is small (about 0.66 MB), but the Linux app binary was 79.6 MiB, 19.6 MiB over the 60 MB budget, before T14 and before any export code was linked (T14 brought it down; the CI figure with the profile applied is not confirmed here); adding a format to an over-budget binary hides the real problem. **Flip condition:** `export-parquet` goes into the `sqler-app` default when, after T14, the Linux release binary of `sqler-app` built with the `export-parquet` feature is under 60 MB, and there is at least the measured Parquet delta (0.66 MB with the T14 profile; use 1 MiB) of room left without it. Until then Parquet is available to a build that asks for it and to the CLI (T4). `export-xlsx` is in the default because its cost is about 0.1 MB.

## Consequences

- Good: both formats stream into a `Write`-only sink with bounded memory (measured at 1M rows); no `arrow-*` crates, no `zip` dependency in the shipped graph, no new licence, no advisory ignore.
- Good: formulas can not be written, and the quote-prefix style keeps formula-like text as text in Excel.
- Good: each format is switched off by a cargo feature, and the contract types are always compiled, so T4 and T9 build against a fixed surface.
- Bad: we maintain about 150 lines of zip and some SpreadsheetML. The tests read it back with `calamine`, and the zip writer has its own unit tests; Excel itself is stricter than `calamine` about package structure, so a maintainer must open a sample file in Excel (and LibreOffice) before release.
- Bad: no ZIP64 and no sheet split: more than 1,048,575 rows or 4 GiB is an error (use `CSV` or `Parquet`).
- Bad: a cancelled or failed export leaves a corrupt partial `.parquet` or `.xlsx` (no footer, no central directory); the ADR-0028 rule applies, the caller deletes the file.
- Bad: for `Parquet`, `Progress.bytes` moves once per row group, not per chunk; a progress bar fed by bytes will be flat for 65,536 rows at a time (`rows` still moves per chunk).
- Bad: on macOS `parquet` brings `chrono` and `core-foundation-sys` into the graph.

**Update (ADR-0032, T9).** The flip condition above was met: `export-parquet` is now in the `sqler-app` default. Measured Linux aarch64 release sizes and the decision are in [ADR-0032](0032-grid-export.md).
