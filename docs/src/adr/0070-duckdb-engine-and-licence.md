# ADR-0070: DuckDB native engine, sandbox and the `webpki-roots` licence question

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers (the owner delegated the licence choice to the team: least complexity and maintenance)

## Context and Problem Statement

[ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) found that a `DuckDB` WASI guest is a no-go and picked the
fallback: a declarative in-repo plugin shell under `plugins/duckdb/` plus a native engine that the host provides through
the driver seam of [ADR-0061](0061-plugin-drivers.md). Two things had to be settled before the engine could land:

1. ADR-0061 stopped P15 on a licence: the `DuckDB` build was believed to bring `webpki-roots` (CDLA-Permissive-2.0,
   not in the `deny.toml` allow list) through `libduckdb-sys` -> build-dependency `ureq` -> `rustls-webpki-roots`.
2. The engine design: threading, streaming, cancellation, sandbox and how several sessions share one file.

## Finding: `webpki-roots` is not in the dependency graph

The ADR-0054 spike used `libduckdb-sys` 1.10506.1, whose non-optional build-dependency `ureq` (feature `rustls`)
enabled `webpki-roots`. The engine uses `duckdb` **1.20000.0** (the newest stable on crates.io on 2026-10-23,
`default-features = false`, feature `bundled` only). Its `libduckdb-sys` 1.20000.0 has **no `ureq` build-dependency**:

- `cargo tree -p sqler-driver-duckdb -e build -i webpki-roots` and `-e normal -i webpki-roots` both fail with
  "package ID specification `webpki-roots` did not match any packages"; `Cargo.lock` has no `webpki-roots` and no
  `ureq` entry.
- `cargo deny check` (advisories, bans, licenses, sources) is clean with `[graph] all-features = true`.

The decision rule of the owner's delegation was "if the DuckDB build does not need `webpki-roots` in the shipped feature
set, prefer no exception". It does not need it, and the graph does not contain it, so **no `deny.toml` exception, no
licence text and no `NOTICE` line are added**, and the licence allow list is unchanged. The note "P15 must stop and ask
the owner" in ADR-0061 is settled by this finding.

If a later `duckdb` bump brings the crate back, the fallback stays as decided with the owner: a single-crate
`exceptions` entry for `webpki-roots` (`CDLA-Permissive-2.0`) with the licence text under `licenses/` and a `NOTICE` line
saying it is build-time only. Never an advisory ignore, never the `rsa` crate.

## Decision

- **Crate.** `sqler-driver-duckdb` on `duckdb` 1.20000.0 with feature `bundled` only (no `httpfs`, `json`, `parquet`,
  `polars` or other extension features). It mirrors `sqler-driver-sqlite`: `Driver` -> `Session` -> `QueryStream` +
  `Introspect`. The public contract is `DRIVER_ID`, `ENGINE_ID` (both `duckdb`) and `DuckDbDriver`.
- **One worker thread per session.** A `duckdb::Connection` is blocking and `!Sync`, and statements borrow it. A
  `std::thread` owns the connection and takes commands over a channel; replies go back on `tokio::sync::oneshot`. Rows
  travel on a bounded `tokio::sync::mpsc` (capacity 2, `blocking_send`), at most 500 rows per `Rows` event. A dropped
  or cancelled stream closes the channel and the worker abandons the result. The futures only wait on channels;
  `close` joins the worker through `spawn_blocking`, so it needs a Tokio runtime like the other drivers.
- **Arrow results, streaming.** Rows are read as Arrow batches (`duckdb::arrow`, re-exported, no direct `arrow`
  dependency) through `Statement::stream_arrow` plus `Statement::step`, so results are fetched lazily and
  `DECIMAL(38)` / `HUGEINT` arrive as `Decimal128` and stay exact (the `rust_decimal` route caps at 28 digits). The
  crate's own `Arrow` iterator panics on a fetch error, so the engine calls `step` itself and maps errors. A statement
  is never run twice to learn its schema. `INSERT`, `UPDATE` and `DELETE` run through `Statement::execute`, which
  reports the changed-row count: such a statement yields no columns and `rows_affected: Some(n)`; with `RETURNING`
  its rows stream as usual.
- **Types.** Boolean, integers (`UBIGINT` above `i64::MAX` as exact decimal), floats (NaN and infinity as text),
  decimals, text, blobs, dates, times, timestamps (`TIMESTAMP WITH TIME ZONE` as offset 0) map to typed values. An
  out-of-range date (`infinity`) and every nested, interval, `UUID` or `ENUM` value become text through Arrow's display
  formatter, with data type `Other`. `sqler-core` has no nested value.
- **Cancel.** The canceller sets a flag and calls `DuckDB`'s interrupt handle, so a long single step (a large aggregate)
  stops as well. The flag is reset when `execute` starts; a cancelled stream yields `Err(Cancelled)` and then `None`, and
  the session stays usable.
- **Sandbox (A1 of the plan).** The host-engine plugin has no guest, so there is no `fs.*` scope. The engine opens with
  `enable_external_access = false`, `autoinstall_known_extensions = false`, `autoload_known_extensions = false`,
  `allow_community_extensions = false` and then `lock_configuration = true`. Reading files, `INSTALL`, `ATTACH` of
  another file and `SET enable_external_access = true` fail. Reversible later by one ADR.
- **One instance per file.** `DuckDB` refuses a second instance on one file. A process-wide registry keyed by the
  canonical path (canonical parent plus file name for a new file) holds a `Weak` to a shared root connection; each
  session gets a `try_clone`. The object browser, a query tab and an export therefore share a file without a lock
  conflict. `:memory:` is private to its session and never shared.
- **Errors never carry the path.** Open failures are the fixed texts "the DuckDB database file is locked by another
  process" and "the DuckDB database could not be opened" (`DriverError::Connection`). Query errors keep `DuckDB`'s text
  with every occurrence of the target (as given and canonical) replaced by `<database>`. `DuckDB` has no credentials.
- **Introspection.** `Database`, `Schema`, `Table`, `View`, `Index` and `Sequence` come from `duckdb_databases()`,
  `duckdb_schemas()`, `duckdb_tables()`, `duckdb_views()`, `duckdb_indexes()` and `duckdb_sequences()`; columns from
  `duckdb_columns()`, keys from `duckdb_constraints()`; every parameter is bound. The default `main` schema is listed
  although `DuckDB` flags it internal; the system schemas are not. Other kinds are `Listing::Unsupported`.
- **Wiring.** The binaries link the engine behind the default-off feature `driver-duckdb` (implies `plugins`), with one
  `cfg` line in each `engines()` ([ADR-0061](0061-plugin-drivers.md), "Binary size and the fallback"). The default
  build and its dependency tree do not change.

## Consequences

- Positive: a working, tested `DuckDB` engine behind a plugin shell, streaming with bounded memory, interruptible, no
  new licence class in the allow list, no exception to maintain.
- Negative: the bundled C++ build adds minutes to every uncached build that compiles the crate (workspace tests and
  `clippy --all-features`), and the engine plus Arrow is large; it stays off by default and the integration step
  reports the binary size. `arrow` brings about 30 new crates to `Cargo.lock`, all permissively licensed.
- Gaps (recorded in the plugin driver docs): one statement per run; a nested value is text; the manifest `dialect` has
  no consumer so a `duckdb` connection is classified `Unknown`; sandboxing is engine-specific and not shown in the grant
  dialog.
- Follow-up: a bump of `duckdb` must re-run `cargo tree -i webpki-roots` and `cargo deny check`.
