# 0032 Grid export: loaded rows or a fresh run of a read statement, writer on the blocking pool, hidden partial file, Parquet in the app default

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

T9 adds **Export Results...** to the results grid and to query results. The writers exist ([ADR-0028](0028-export-writer-api.md), [ADR-0029](0029-parquet-and-xlsx-writers.md)); what is open is where the rows come from, where the writer runs, how a failed or cancelled export leaves no damaged file, which statements may be run a second time, and whether `export-parquet` joins the `sqler-app` default now that the Linux binary has room (the flip condition of ADR-0029). A result can have millions of rows, but the grid holds only the loaded pages ([ADR-0027](0027-bounded-result-model.md)).

## Decision drivers

- Memory independent of the row count, on the UI side as well.
- The UI thread never blocks on disk or on a database; no `tokio::spawn` on the GPUI thread ([ADR-0007](0007-desktop-binary-and-ui-tokio-bridge.md)).
- Never run a write twice, and never show the SQL, a path or a credential in an error text (the database's own message is shown, as in the query tab).
- An existing file is replaced only by a complete one.
- Testable without a window.

## Considered options

1. Export from the tab's own session (lend it, [ADR-0024](0024-query-execution.md)).
2. Export only the rows loaded in the grid.
3. Two sources: the loaded rows, and a fresh run of the statement on a new short-lived session, limited to one read statement.

## Decision outcome

Chosen: option 3. Option 1 blocks the editor for the whole export and is impossible while a paused statement holds the session (the credit gate of ADR-0027). Option 2 can not export a result larger than a page.

**Pipeline.** `sqler_ui::export` (GPUI-free; only `export::view` needs the `gui` feature) joins one *producer* and one *writer* with a bounded `tokio::sync::mpsc` channel of `Feed` messages (`Columns`, `Rows`, `Done`, `Failed`), capacity `FEED_CAPACITY` = 4 batches. A slow disk therefore stops the producer, and memory is a few batches whatever the row count. A test pushes 1000 x 1000 rows and checks that no more than `FEED_CAPACITY + 2` batches are in flight.

**Writer.** `write_feed` runs on tokio's blocking pool (`SessionBridge::blocking`) and uses `blocking_recv`. It drives `sqler_export::Exporter` into a `PartialFile` (a hidden sibling `.<name>.<pid>-<job>.partial`, created with `create_new`) and, only after `finish`, fsyncs and renames it over the target. Failure, cancel or a dropped writer deletes the partial file. The rename replaces the directory entry: a target that is a symlink is replaced by a regular file (the file it pointed to is untouched), and on Unix a replaced regular file passes its permissions to the new file (best effort); any other new file gets the process default (umask). This is the protocol of `sqler-cli export`, re-implemented because the CLI type is private. Progress is coalesced before it reaches the UI.

**Producers.**

- *Loaded rows.* The `GridModel` belongs to the UI thread, so `LoadedCursor` copies rows out in slices with a time budget and hands them to the channel; a selection copies the selected rectangle in display order. The loaded-rows scope follows the grid's own sort and filter too: with either set, the facts carry the displayed model rows (`GridFacts::displayed`) and the copy is a `Picked` of all columns in display order, so the file and the dialog's row count match what is on screen; with none set it is the first N model rows. A fresh run ignores sort and filter. The cursor keeps the table entity alive, so the file reflects the rows at export time.
- *Fresh run.* `fresh_rows` opens a short-lived session through the `Connector` (as `open_data` in the object browser does), runs the tab's statement and streams the batches with `pump_stream`, without loading them into the grid. It is allowed only when `sqler_sql::classify_each` gives exactly one statement of class `Read`, the class recorded when the tab ran it is also `Read`, and the connection has a dialect (`fresh_gate`); `Workspace::fresh_check` then checks that the tab still has its connection. Anything else (a write, DDL, a script, an unclassified statement) is never run a second time; the dialog shows the reason and offers the loaded rows only. A fresh run can see newer data than the grid, and session-local objects (temporary tables, `SET` values) are missing on the new session, so it fails with the driver error; the user guide says so. The gate is lexical (`Read` means no write syntax was found), so a second layer sits behind it: before the statement runs, the fresh session is put in a read-only mode (`PRAGMA query_only = ON` on `SQLite`, `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` on PostgreSQL, `SET SESSION TRANSACTION READ ONLY` on MySQL), so the database refuses a write that the classifier missed. `DuckDB` and SQL Server have no such session setting; for them only the lexical gate applies, and a view, user-defined operator or cast that writes is a known blind spot.

**Cancel and lifetime.** `ExportStop` combines a `CancelToken` with a watch channel; it stops the producer (also during connect and execute), the channel and the writer. Closing the tab and quitting cancel the tab's export. A process that is killed can leave a hidden `.partial` file; the name makes it recognisable.

**Errors.** `ExportFailure` has a user text per kind (`Cancelled`, `Connect`, `Driver`, `Create`, `Write`, `Format`, `NoResultSet`, `SourceEnded`, `ResultChanged`, `Lost`). `Connect` and `Driver` carry the text the connect pipeline or the driver shows the user (as the query tab does), so the user sees the real cause, for example the database's `syntax error at row 40,000`. They are built from the `Display` of the error only, through `connect_text` and `driver_text`, which map the variants that can echo the target, a path or a secret to fixed texts (`Tunnel`, `Target`, `Secret`, a driver `invalid ... target` and a library `Backend` error). No variant carries the statement, the target path or a credential. The one exception is the database's own message, which can quote a token of the statement (as in the query tab). `ExportError::InvalidOptions` and `Sql` map to fixed texts because they echo option and identifier values.

**Where state lives.** The dialog, the form, the running job and its progress are headless state in `Workspace` (`workspace/export.rs`), which returns two effects, `PickExportPath` and `RunExport`. The native save dialog, the overlay and the progress bar are in `workspace/view/export.rs` and `export/view.rs`. Commands: `grid:export` and `export:cancel`.

### Parquet in the `sqler-app` default

ADR-0029 kept `export-parquet` out of the default until, after T14, the Linux release binary built with it is under the 60 MB budget with at least 1 MiB of room left without it. T9 turns that into a number: `export-parquet` goes into the default iff the Linux `sqler` built **with** it is at most **61,865,984 bytes** (the budget of `size.yml`, 62,914,560, minus 1 MiB).

Measured in Docker (`rust:1.99.1`, Linux **aarch64**, the apt packages of AGENTS.md section 3, `cargo build --release -p sqler-app --locked`, the T14 profile `strip = "symbols"`, `codegen-units = 1`, on the T9 tree, so the export pipeline is linked in both builds):

| Build | `sqler` bytes | MiB |
| ----- | ------------- | --- |
| `--no-default-features --features driver-sqlite,driver-postgres,driver-mysql,export-xlsx` | 46,626,808 | 44.5 |
| default + `export-parquet` | 47,282,168 | 45.1 |
| Parquet delta | 655,360 | 0.63 |

The delta matches the 0.66 MB projected in ADR-0029. The same two builds for Linux **x86_64** (Docker `--platform linux/amd64`, emulated on the arm64 host, same image, packages and profile):

| Build (x86_64, emulated) | `sqler` bytes | MiB |
| ------------------------ | ------------- | --- |
| without Parquet | 54,165,824 | 51.7 |
| with Parquet | 54,947,264 | 52.4 |
| Parquet delta | 781,440 | 0.75 |

The with-Parquet figure is 6,918,720 bytes (6.6 MiB) under the limit of 61,865,984. It is consistent with `size.yml` on `main` before T9, which reported 53,703,760 bytes for x86_64 without Parquet and without export code (the T9 pipeline and XLSX add about 0.46 MB). The emulated build runs the same compiler and flags, but the runner of `size.yml` is authoritative.

**Decision: flip.** `export-parquet` is in the `sqler-app` default. **The `Binary size` run of the pull request is the final check.** If its Linux figure is above 61,865,984 bytes, revert the single line in `crates/sqler-app/Cargo.toml` (`default = [..., "export-xlsx", "export-parquet"]` back to `[..., "export-xlsx"]`); the dialog already lists only the formats the build has (`available_formats`), so nothing else changes, and record the figure here. The macOS and Windows binaries are far under the budget.

## Consequences

- The dialog offers CSV, JSON, SQL, Markdown, Excel and Parquet in a default build.
- A second round trip to the database exists only for one statement classified as a read; every other result exports its loaded rows.
- Memory of an export is a few batches; the UI thread does at most one time-boxed slice of copying per 1 ms timer tick.
- Throughput (release, this machine, 1,000,000 rows x 6 columns, [benchmarks](../benchmarks.md)): loaded to CSV 4.34 M rows/s, loaded to Parquet 3.32 M rows/s, fresh `SQLite` to CSV 0.86 M rows/s (bound by the query).
- The native save dialog, the progress bar, Cancel, and opening the files in Excel, LibreOffice and a Parquet reader are maintainer checks.
- A selection export follows the grid's own row order (sort and filter included), read from `grid.rows()` in `grid_facts`. A selection that holds a position the grid cannot map to the model is refused, never shortened.
