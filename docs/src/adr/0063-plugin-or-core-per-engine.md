# ADR-0063: Plugin or core crate, per engine (SQL Server and DuckDB)

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers; DuckDB out of 1.0 is the product owner's decision (owner decision (2026-10-23): DuckDB is out of v1.0, recorded in [ADR-0070](0070-duckdb-engine-and-licence.md), amendment 2)

## Context and Problem Statement

The M6 plan says the two database engines that the first-party plugins need, SQL Server and `DuckDB`, are "promoted to
core crates if the API proves insufficient (record why)". Both were built in M6 as plugins with a host-provided engine
([ADR-0061](0061-plugin-drivers.md)): P16 ([ADR-0071](0071-sql-server-plugin.md)) and P15 (ADR-0070). Both items listed
what the plugin API could not express. This ADR decides, per engine, whether it stays a plugin or becomes a core crate,
and what a later promotion would need.

Facts that bind the decision:

- [ADR-0054](0054-plugin-runtime-and-driver-feasibility.md): neither engine runs as a `wasm32-wasip2` guest (the `duckdb`
  crate does not build for the target, and the bundled C++ build needs the legacy wasm exception encoding that the
  runtime rejects; `tiberius` needs a `rustls-native-certs` patch and the WASI socket
  surface is not enough). The plugin route for a database engine is therefore a **host-provided native engine** behind
  a declarative plugin, never a guest. A guest driver stays possible for pure-protocol engines.
- [ADR-0069](0069-binary-size-budget-70-mb.md): the release binary budget is 70 MB = 73,400,320 bytes.
- Owner decision: `DuckDB` is out of version 1.0.

## Measured cost (macOS arm64 unless noted, `cargo build --release -p sqler-app --locked`)

| Build | Bytes | Delta |
| ----- | ----- | ----- |
| default | 38,186,592 (P7 tree; 38,507,056 on the CI macOS runner at `a141bb7`) | |
| `--features plugins` (wasmtime Cranelift, HTTP egress) | 49,272,272 (CI macOS: 50,644,320) | +11,085,680 (CI +12,137,264) |
| `plugins` plus `driver-mssql` | +1.08 MB over `plugins` (P16 measurement) | small |
| `plugins` plus `driver-duckdb` | 95,180,224 against 49,757,680 for `plugins` | +45,422,544 (43.3 MiB) |

Linux, from `Binary size` run 39972514174 at `a141bb7`: default 63,330,024 bytes (60.4 MiB), `--features plugins`
79,908,200 (76.2 MiB, +16,578,176, over the budget). Neither engine has a Linux or Windows figure of its own: `size.yml`
measures the default build and the `plugins` build only. Engine tests: SQL Server runs against the 2022 container in the
CI `db` job; `DuckDB` runs in the manually dispatched `duckdb` job only.

## Decision

### SQL Server: stays a plugin (host-provided engine)

- It ships as the plugin `plugins/sqlserver` over the native engine `sqler-driver-mssql`, linked only with the default-off
  feature `driver-mssql`. The feature is not part of the default build or of the plugin host's size decision.
- Why not core now: (1) the default binary must not carry `tiberius` and its TLS tree before the engine has been run
  against a real server by the owner (the real-instance check is a Pending (maintainer) item); (2) the gaps below are
  work in `sqler-sql`, the connect pipeline and `sqler-driver-api`, which is the same work whether the crate is core or
  a plugin; promoting first would only move the unfinished driver into the default build. The size of the engine
  itself (+1.08 MB on macOS) is **not** the reason; it would fit the budget.
- What the plugin API covered: registration of the driver id, the capability `driver.host:mssql` and its grant, the
  unavailable text when the engine is not compiled in, the driver list in the connection dialog, the connect, query,
  stream, cancel and catalog calls through the `Driver` trait, and the guards of the built-in drivers (production gate,
  AI approval) applied unchanged.
- What it could not express (the gaps, from [Plugin drivers](../plugin-development/drivers.md#sql-server)):
  - (fixed by [ADR-0073](0073-tsql-dialect-and-manifest-dialect.md)) no T-SQL dialect (`Dialect::from_driver_id("sqlserver")` was `None`; the manifest `dialect` key had no consumer):
    statements are not split, not classified and have no placeholders, so **every statement asks for confirmation on a
    `prod` connection**, even `select 1`, and the fresh-run export, the SQL `INSERT` export and the AI query tools
    refuse the driver;
  - `rows_affected` is `None` (the stream does not expose the `DONE` counts), so **inline grid editing cannot be applied
    on SQL Server**: the apply path rolls back safely and writes nothing;
  - a host-driver plugin cannot supply catalog SQL (the engine reads `sys.*` itself and browses the current database);
  - the connect pipeline injects no keychain password and opens no SSH tunnel for `sqlserver://`;
  - `net` scopes do not apply to host engines (the engine opens its own socket);
  - cancel ends the session (the connection is closed, ADR-0071);
  - `money` arrives as a float and only the first result set of a batch is returned.
- Native engines are needed because the WASI guest route is NO-GO (ADR-0054). A pure-Rust protocol client could become a
  guest driver only when WASI sockets and TLS roots work for it; this was not found to hold for `tiberius`.

### DuckDB: stays a plugin, disabled, out of 1.0

- It is the declarative plugin `plugins/duckdb` over `sqler-driver-duckdb` behind the feature `driver-duckdb` (implies
  `plugins`, off by default, never in a release artifact, installer or update manifest). It is documented as "not in
  1.0"; this ADR does not promise it.
- Why not core: the bundled C++ engine adds **+43.3 MiB** (45,422,544 bytes on macOS), which puts a `plugins` build
  21,779,904 bytes over the budget, and about 8 CPU minutes of build time uncached; the CI `duckdb` job is manual-only
  for that reason. A core crate in the default build is impossible inside 70 MB.
- API gaps found (from [Plugin drivers](../plugin-development/drivers.md#api-gaps)): G1 (fixed by [ADR-0073](0073-tsql-dialect-and-manifest-dialect.md) for the named dialects) the manifest `dialect` key had no
  consumer (no splitting, no placeholders, class `Unknown`, so every run on `prod` asks for confirmation; fresh-run
  export, SQL `INSERT` export and AI query tools refuse the driver); G2 a host-engine plugin is purely declarative
  (catalog SQL, type mapping and sandbox live in the engine crate); G3 no session read-only statement; G4 `sqler-core`
  has no nested, interval or UUID `Value` (they arrive as text, `DataType::Other`); G5 `fs.*` scopes do not apply to host
  engines, so the engine sandbox is invisible in the grant dialog; G6 per-file instance sharing is the engine's own job;
  G7 size and build cost.
- It may return after 1.0 as a separate download; that is a new decision and a new ADR.

### What a later promotion to a core crate would need

For either engine, in this order:

1. A dialect in `sqler-sql` (done for T-SQL by ADR-0073; `Dialect` variant, lexer rules, statement classification, quoting, placeholders, DDL), then
   wiring the manifest `dialect` key (G1) so the production gate classifies instead of always asking.
2. `rows_affected` in the driver stream for SQL Server (inline editing); a read-only session guard (G3) for `DuckDB`.
3. Core value types for the types that arrive as text today (G4, `DuckDB`), and decimal for `money` (SQL Server).
4. The connect pipeline: keychain password injection and the SSH tunnel for `sqlserver://`.
5. A size answer: SQL Server fits (+1.08 MB macOS); `DuckDB` does not, so a promotion means a separate build or download,
   not the default binary.
6. A real-server run by the owner for SQL Server.

## Consequences

- Both engines stay behind default-off features and plugin shells; the default binary and the `plugins` feature decision
  ([ADR-0060](0060-plugins-in-the-workbench.md), [ADR-0069](0069-binary-size-budget-70-mb.md)) are unchanged.
- The M6 DoD wording "promoted if the API proves insufficient" is answered: the API was insufficient in places (the
  gaps), but promotion would not fix them, because they are core-side; they are listed for M7 and later.
- Users of SQL Server get connect, browse and run, with a confirmation on every `prod` statement and no inline editing.
