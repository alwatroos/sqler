# Drivers

A plugin can add a database driver. The decision is recorded in
[ADR-0061](../adr/0061-plugin-drivers.md). A plugin driver appears in the connection form
next to the built-in ones and is used through the same run flow, so every statement still
goes through the statement classifier and the production confirmation. A plugin driver has no
way to write around them.

Plugin drivers need a build with the `plugins` feature (off by default in the shipped binary).

## Two kinds

| | Host driver | Guest driver |
| - | - | - |
| Manifest | `kind = "host"`, `engine = "<engine-id>"` | `kind = "guest"` |
| Capability | `driver.host:<engine-id>` | `driver.guest` |
| Needs `[entry]` | no | yes |
| Who connects | the SQLer binary, with a native engine | your component |
| What your component sees | nothing | the connect parameters |

The driver id (`[[drivers]] id`) is the id stored in `connections.toml`. It is not
namespaced. The ids `sqlite`, `postgres` and `mysql` are reserved: a plugin driver with one of them is unavailable, also in a build without that built-in. When two
plugins claim one id, the first loaded wins.

## Host drivers

A host driver is a declarative shell around an engine that the binary was built with. See
the example in [Manifest](manifest.md). If the build does not include the engine, the driver
is listed as unavailable with a clear text. Host engines receive the whole connection
config; the plugin never does.

## Guest drivers

A guest driver exports the WIT `driver` interface, and optionally `driver-catalog` (world
`driver-catalog-plugin`, package `sqler:plugin@0.1.1`) for the object browser. Without
`driver-catalog` the driver works but has no introspection.

```toml
[plugin]
id = "my-db"
name = "My DB"
version = "0.1.0"
api = "0.1"
capabilities = ["driver.guest"]

[entry]
component = "my_db.wasm"

[[drivers]]
id = "my-db"
name = "My DB"
kind = "guest"
```

### What your driver receives

Only `connect(driver, connect-params)`: host, port, user, password, database and the query
pairs of the connection target as `options`. A target that is not a URL arrives as
`database`. Your driver never receives the SSH tunnel secret, TLS file paths, other
connections or the environment. It has no sockets: the only network access is HTTP under
`net.http`.

The user is told in the grant dialog that the plugin receives the host, username and
password of every connection that uses its driver. Treat the password accordingly: do not
log it and do not put it in an error message (a message that carries the password value, raw or
percent-encoded, or mentions the word, is replaced by a fixed text).

### Streaming

`execute` returns `rows(stream)` or `affected(n)`. The host reads the stream with
`next-batch(500)`. A batch must have at most 500 rows, each as wide as the column list, and
the whole result stays under the result byte cap; otherwise the stream fails with "the
plugin driver sent malformed rows". `none` ends the stream. An empty batch does not: the host
asks again, and a cancel or the deadline stops a guest that never answers `none`.

### Cancel, traps and sessions

There is no `cancel` export. Cancel stops the running call by dropping the plugin instance.
A cancel between batches only ends the stream. A cancel during a call, a trap or a timeout
ends every open session of the plugin; the next statement on such a session fails with
"the plugin driver session ended (the plugin was restarted, disabled or its permissions
changed)", and the user connects again. Revoking `driver.guest` or disabling the plugin has
the same effect.

## Error texts

The user sees these fixed texts, never wasmtime text.

| Situation | Text |
| - | - |
| Plugin disabled | driver `{d}` comes from plugin `{p}`, which is disabled |
| Permission not approved | driver `{d}` needs the permission `{cap}` of plugin `{p}`, which is not approved |
| Engine not in this build | driver `{d}` needs the `{e}` engine, which this build does not include |
| Id already provided | driver `{d}` is already provided by plugin `{q}` |
| Trap | the plugin driver crashed |
| Timeout | the plugin driver did not answer in time |
| Memory limit | the plugin driver ran out of memory |
| Plugin gone | the plugin driver is not available |
| Bad batch | the plugin driver sent malformed rows |
| Session over | the plugin driver session ended (the plugin was restarted, disabled or its permissions changed) |
| Bad catalog answer | the plugin driver sent a malformed catalog answer |

Your own error messages reach the user after the same scrub as `query` errors.

## Where the driver shows up

The connection form offers the plugin's driver id next to the built-in ones. The list is
refreshed when the Plugins view changes something (a plugin is switched on or off, or a
permission is approved or revoked), so a new driver appears without a restart; a form that is
already open keeps the list it was built with. A driver that is not available (disabled, not
approved, no engine) is not offered, and a saved connection that uses it fails with the text
above. The `sqler-cli` binary built with its `plugins` feature (off by default) offers the
drivers of approved plugins in `<config>/plugins` too, honours the stored approvals and the
plugins switched off, and never asks.

## Limits

- The password from the keychain is put into the target by the connect pipeline, which knows
  the URL schemes `postgres`, `postgresql`, `mysql` and `mariadb`. A driver with another
  scheme gets the password only when the user wrote it into the target, and cannot use an SSH
  tunnel.
- A plugin driver is reached through the registry only, so the statement classifier and the
  production confirmation always run before a statement reaches it.

## DuckDB

> **Not in 1.0.** `DuckDB` is out of version 1.0 (ADR-0070, amendment 2). The code below stays in the repository,
> disabled: the feature `driver-duckdb` is off by default, is never part of a release artifact, installer or update
> manifest, and is built in CI only by the separate `duckdb` job. It may return after 1.0 as a separate download.

`DuckDB` is the first host driver in the repository (ADR-0070). It has no guest: the plugin `plugins/duckdb` is a manifest only (`kind = "host"`, `engine = "duckdb"`, capability
`driver.host:duckdb`, no `[entry]`), and the engine is the crate `sqler-driver-duckdb`, a native driver on the
`duckdb` crate with the `bundled` feature.

### How the shell maps to the engine

- The binary registers the engine under the id `duckdb` only when it is built with the feature `driver-duckdb` of
  `sqler-app` or `sqler-cli` (off by default; it implies `plugins`). Without it the driver is listed as unavailable
  with the "Engine not in this build" text.
- The plugin must be installed (copy `plugins/duckdb/sqler.toml` to `<config>/plugins/duckdb/`) and
  `driver.host:duckdb` approved.
- The target is `:memory:` (private to its session) or a file path (created if missing). The engine ignores
  `config.driver`. One `DuckDB` instance is shared per file inside the process, so the object browser, a query tab
  and an export can use one file at once.
- Sandbox, set when the database opens and then locked: external file access off, extension autoinstall and
  autoload off, community extensions off. `read_csv` of a file, `INSTALL` and `SET enable_external_access = true`
  fail. This is engine configuration, not a grant, so the grant dialog does not show it.
- Errors never carry the database path: connect errors use two fixed texts, and query errors have the path replaced
  by `<database>`.

### API gaps

What the plugin API could not express for this driver, as found while building it.

| ID | Gap |
| - | - |
| G1 | The manifest `dialect` key has no consumer. `Dialect` covers only the built-in driver ids, so a `duckdb` connection gets no statement splitting, no placeholders and the `Unknown` statement class: every run on a `prod` connection asks for confirmation, even a `SELECT`. Fresh-run export, the SQL INSERT export and the AI query tools refuse the driver. Run one statement or a selection; a multi-statement text fails with the `DuckDB` error. |
| G2 | A host-engine plugin is purely declarative. Catalog SQL, type mapping and the sandbox live in the engine crate, so the plugin API validates registration and grants only. |
| G3 | There is no session read-only statement for `DuckDB` (`read_only_statement`). |
| G4 | `sqler-core` has no nested, interval or UUID `Value`. Lists, structs, maps, unions, intervals, enums and UUIDs arrive as text with `DataType::Other`. |
| G5 | `fs.*` scopes do not apply to host engines. Sandboxing is engine specific and invisible in the grant dialog. |
| G6 | An engine has to build its own per-file instance sharing across sessions; the driver API offers none. |
| G7 | Size and build cost of a native engine: see "Size and build cost". |

### Size and build cost

Measured on macOS (aarch64), `cargo build --release -p sqler-app --locked`:

| Features | Bytes | MiB |
| -------- | ----- | --- |
| `plugins` | 49,757,680 | 47.5 |
| `plugins,driver-duckdb` | 95,180,224 | 90.8 |

The engine adds 45,422,544 bytes (43.3 MiB). With it the binary is 21,779,904 bytes over the 73,400,320 byte budget
([ADR-0069](../adr/0069-binary-size-budget-70-mb.md)), so `driver-duckdb` stays off in the default build and a build
with it is outside the size budget by design. The default build does not change (the `deny` job asserts that `duckdb`
is not in the default `sqler-app` or `sqler-cli` tree).

The bundled C++ engine also adds build time: about 490 s of CPU for an uncached debug build (64 s wall on a 14-core
laptop), so a few minutes on a CI runner. See [ADR-0070](../adr/0070-duckdb-engine-and-licence.md), "CI cost".

## SQL Server

The in-repo plugin `plugins/sqlserver` is a host-driver shell around the native engine `mssql`
(`crates/sqler-driver-mssql`, `tiberius` with `rustls`). It is the worked example of a host
driver; the decisions are in [ADR-0071](../adr/0071-sql-server-plugin.md).

**Build.** The engine is linked only when `sqler-app` or `sqler-cli` is built with the feature
`driver-mssql` (default off; it implies `plugins`). The plugin itself is staged by
`cargo run -p xtask -- plugins build` as `target/plugins/<profile>/sqlserver/sqler.toml` (no
component). Copy that folder to `<config>/plugins/sqlserver/` and approve `driver.host:mssql`.
Without the feature the driver is listed as unavailable with the fixed "engine, which this build
does not include" text.

**Manifest.**

```toml
[plugin]
id = "sqlserver"
name = "SQL Server"
version = "0.1.0"
api = "0.1"
capabilities = ["driver.host:mssql"]

[[drivers]]
id = "sqlserver"
name = "SQL Server"
kind = "host"
engine = "mssql"
dialect = "tsql"
```

**Target and login.** `sqlserver://[user[:password]@]host[:port][/database]` (alias `mssql://`),
SQL login only, no query keys.

**TLS.** `ConnectionConfig::tls` maps as follows; none means `prefer`.

| Mode | Result |
| ---- | ------ |
| `disable` | no encryption (a server that forces encryption refuses) |
| `prefer` | encrypted, certificate not checked |
| `require` | encrypted, certificate not checked |
| `verify-ca` | `Unsupported` ("use verify-full or require") |
| `verify-full` | chain checked against the CA file, else the OS roots; the host name is checked. With `server_name` set, that name is checked instead of the dial host (this works with `tiberius` 0.13) |
| client certificate or key | `Unsupported` |

**Cancel.** A cancel closes the connection (the server rolls back an open transaction); the next
statement of that session fails with "the SQL Server connection was closed by a cancel; connect
again".

**API gaps** (none is fixed by this item).

- No T-SQL dialect exists, so `Dialect::from_driver_id("sqlserver")` is `None`: every statement
  counts as not read-only, a production connection asks for confirmation of every statement
  (including `select 1`), and the manifest `dialect` is not used.
- A host-driver plugin cannot supply catalog SQL; the engine implements introspection itself from
  `sys.*` and browses only the current database.
- The connect pipeline injects no keychain password and opens no SSH tunnel for `sqlserver://`: the
  password must be in the target, and a tunnel cannot be used.
- `net` scopes do not apply to host engines; the engine opens its own socket.
- `rows_affected` is `None`: the statement stream does not expose the `DONE` counts.
- Cancel ends the session (see above), unlike the other drivers.
- `money` arrives as a float (lossy); the first result set of a batch is returned and later ones
  are dropped.

**Pending (maintainer).** A run against a real SQL Server instance (not only the CI container,
SQL Server 2022 Developer edition): connect with SQL login, TLS modes against the server's own
certificate, the object browser, a cancel, and an Azure SQL or older server for the TLS handshake.
