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

Reserved for the DuckDB host-engine plugin (P15).

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
