# ADR-0071: SQL Server as an in-repo plugin with a host-provided engine

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

[ADR-0054](0054-plugin-runtime-and-driver-feasibility.md) found that a SQL Server driver cannot ship as a
`wasm32-wasip2` guest (the `rustls-native-certs` patch and the WASI socket surface) and named the fallback: a native
engine linked into the binary, reached by a declarative plugin. [ADR-0061](0061-plugin-drivers.md) built that seam
(`driver.host:<engine>`, `HostEngines`). This item builds the SQL Server side of it. Three questions needed a decision:
is this a plugin or core, what does cancel do, and which TLS stack is in effect.

## Decision

### Plugin versus core

- `crates/sqler-driver-mssql` holds the native engine (`tiberius` 0.13, `rustls` with the `ring` provider). `sqler-app`
  and `sqler-cli` link it only behind a default-off feature `driver-mssql` (which implies `plugins`), through one `cfg`
  registration in `engines()`.
- `plugins/sqlserver/` is a manifest-only shell (no `[entry]`, capability `driver.host:mssql`, driver id `sqlserver`).
  The user approves the capability like any other. Without the feature the driver shows the fixed "needs the `mssql`
  engine, which this build does not include" text.
- It is a plugin (not a built-in driver) because the default binary must not carry `tiberius` and its TLS tree: the
  `deny` job asserts there is no `tiberius` line in the default app and CLI trees.
- The cost is real and recorded as API gaps in [Plugin drivers](../plugin-development/drivers.md#sql-server): there is
  no T-SQL dialect (fixed by [ADR-0073](0073-tsql-dialect-and-manifest-dialect.md); the production gate confirmed every statement), a host-driver plugin cannot supply catalog SQL,
  the connect pipeline injects no keychain password and opens no SSH tunnel for `sqlserver://` (fixed, see
  Amendment 1), `net` scopes do not apply to host engines, and `rows_affected` is `None`. None of them is fixed here. One consequence: inline editing in
  the results grid cannot be applied on SQL Server until the driver reports a changed-row count. The apply path sees
  `rows_affected` as `None` and rolls back safely, so nothing is written.

### Cancel closes the connection

A cancel while a statement is active ends the stream with `Cancelled` and drops the client, which closes the socket. The
server ends the statement and rolls back an open transaction. The next `execute` on that session fails with
"the SQL Server connection was closed by a cancel; connect again". This follows the guest-driver precedent of ADR-0061
(a cancel in the middle of a call ends the session). Rejected: a transparent reconnect, because a later write could
autocommit outside the transaction the user believes is open; `KILL <spid>`, because it needs `ALTER ANY CONNECTION`.

### One TLS stack in effect: ring beside aws-lc-rs

`tiberius`' `rustls` feature pulls `tokio-rustls` with its default features, so the `aws-lc-rs` crate is compiled into a
build with `driver-mssql`. It is already in `Cargo.lock` for `sqlx-mysql` ([ADR-0018](0018-rsa-alternatives.md)), so
the lock gains no new C code and the licence and advisory checks are unchanged. With both providers compiled in,
`rustls` no longer picks a default on its own: code that builds a client config without naming a provider panics. We
measured it: `sqlx` verify-ca against PostgreSQL panicked when run together with `sqler-driver-mssql` in one build and
passed once ring was installed. Therefore the engine installs `ring` as the process default
(`sqler_driver_mssql::install_ring_provider`), and `sqler-app` and `sqler-cli` call it at the start of `main` when the
feature is on (and again in `engines()`), before anything can connect. `reqwest` 0.12 already names `ring` itself when
no default is installed. aws-lc-rs is never the provider in use, so exactly one TLS stack is in effect. This is
reversible: an upstream issue asking `tiberius` to drop the default features, or stopping the item.

### Other decisions

- Ids: plugin and driver `sqlserver`, engine `mssql`; the URL scheme `sqlserver://` (alias `mssql://`).
- SQL login only; Windows authentication and Azure AD are out of scope.
- TLS: `Disable` off, `Prefer` and `Require` encrypt without checking the certificate, `VerifyFull` checks the chain
  (CA file or OS roots) and the host name (with `server_name`, that name instead of the dial host), `VerifyCa` and client
  certificates are `Unsupported`.
- The database tests follow [ADR-0009](0009-postgres-driver-tls-db-tests.md): a script starts a TLS-forced container
  (`sqler-test-mssql`), tests skip without `SQLER_TEST_MSSQL_URL`, and the CI `db` job requires `mssql`.
- The scrub of plugin error text learns the SQL Server wording ("was denied", "principal").
  It covers guest driver errors and, through the host-engine wrapper (`HostEngineDriver`), the native engine's
  connect, query, stream and catalog errors: a login failure or a denial becomes the fixed text, and accounts, hosts
  and paths are replaced. The engine itself replaces the dial host and a routing redirect target. Row data is not
  scrubbed.
- Date and timestamp parameters are bound as `YYYYMMDD` and ISO 8601 with `T`, so the session `DATEFORMAT` and language
  do not change them.

## Consequences

- A build with `driver-mssql` carries `tiberius` and the `aws-lc-rs` crate; the release size figures are in the
  CHANGELOG entry and the pull request (macOS, measured with `plugins` and with `plugins,driver-mssql`).
- The CI `db` job pulls a 1.5 GB image and starts SQL Server in 30 to 60 seconds.
- A run against a real SQL Server instance by a maintainer is pending (the container tests run against SQL Server 2022
  Developer edition).

## Amendment 1 (2026-10-23): keychain password and SSH tunnel for `sqlserver://`

- Status: accepted. This fixes the API gap "no keychain password and no SSH tunnel for `sqlserver://`" named above.
- **Pipeline.** The connect pipeline knows the schemes `sqlserver` and `mssql` (default port 1433, case-insensitive).
  It puts the keychain password (or a typed one) into the target as percent-encoded userinfo, and for an SSH tunnel it
  rewrites the dial host to `127.0.0.1:<port>`. The engine parses the result as before. No new code path was added.
- **`?user=` is moved.** A stored target `sqlserver://host/db?user=name` gets its user moved into the userinfo, so the
  engine never sees a query key. A user already in the userinfo wins. Rejected: failing on `?user=`, because that would
  break the stored-target convention of [ADR-0011](0011-connection-store-format.md) for one driver.
- **No URL TLS for T-SQL.** TLS comes only from `ConnectionConfig::tls`. A `sslmode` key in the target is not lifted
  into a TLS mode; it passes through and the engine refuses it by name.
- **Tunnel and `verify-full`.** `server_name` is the original host, so `verify-full` checks that name and not
  `127.0.0.1`. The Windows refusal of `verify-full` through a tunnel stays: it is raised before anything opens.
- **Host-engine scrub.** Because a keychain password now reaches a host engine's target, `HostEngineDriver` denies any
  connect, query, stream or catalog error text that contains the password, raw or percent-encoded (the guest adapter
  already did this).
- **No tunnel container test.** No existing setup reaches a database through `sshd`, and the SSH and SQL Server test
  containers share no network. Fake tunnels cover the pipeline. A real check through an SSH tunnel and against a real
  SQL Server instance is pending (maintainer).

## Amendment 2 (2026-10-24): rows affected and the limits that stay

- Status: accepted. This fixes the API gap "`rows_affected` is `None`", so inline grid editing now applies on SQL Server.
  The amendment number may be renumbered at merge if another amendment took 2; the content does not change.
- **What `tiberius` 0.13 gives.** `QueryStream` (from `simple_query` and `query`) yields only metadata and rows and skips
  every `DONE` token. `execute` returns the counts but drops the rows. `Command::exec` (a named-procedure RPC) streams
  metadata, rows **and** one `RowsAffected` item per `DONE` or `DONEINPROC`. `TokenDone::rows()` returns 0 when the
  count flag is unset, and the status bits are not public, so the driver cannot tell "0 rows" from "no count" by itself.
- **Mechanism.** A text whose leading keyword (after whitespace, `;`, `--` and nested `/* */` comments) is
  `INSERT`, `UPDATE` (not `UPDATE STATISTICS`), `DELETE`, `MERGE` or `WITH`, and that is confidently one statement,
  runs as `sp_executesql` through `Command` (`@stmt`, `@params`, `@P1..@Pn`). Its rows still stream, so an `OUTPUT`
  clause works, and its count is the single `RowsAffected` item. "Confidently one statement" is a token check that
  ignores strings, comments and quoted names: no `;` followed by more text, no word from a list of state or flow
  statements (`USE`, `DECLARE`, `CREATE`, `ALTER`, `DROP`, `EXEC`, `BEGIN`, `IF`, `WHILE`, `COMMIT`, ...), no `SET` beyond
  those of `UPDATE ... SET`, and no `INTO` other than `INSERT INTO` / `MERGE INTO` (so `SELECT ... INTO #tmp` is out).
  `UPDATE ... SET`, `INSERT ... SELECT` and `WITH ... INSERT/UPDATE/DELETE/MERGE` pass. A text that fails the check
  keeps the old plain-batch route and reports `None`, so the grid apply rolls back. Every other statement keeps its
  path (a plain batch, or a parameterised query) and reports `None`. Rejected:
  `execute` for DML, because it drops `OUTPUT` rows; and appending `SELECT @@ROWCOUNT`, because it rewrites the user's
  SQL and adds a result set.
- **When the count is `None`.** Zero count items (a session `SET NOCOUNT ON` suppresses them), more than one (a trigger
  that counts its own statements, a batch or a procedure with several statements), and every statement that is not
  routed as DML. The driver never guesses. The apply path treats `None` as an unverifiable row count and rolls back, so
  the safe default of the grid editor is unchanged. A trigger is fixed by `SET NOCOUNT ON` as its first statement.
- **Plain DML moves to RPC.** A text routed as DML without parameters now also runs through `sp_executesql`. The scope
  of `SCOPE_IDENTITY()` was already per batch, so nothing is lost; existing temporary tables and an open
  `BEGIN TRANSACTION` carry over (the transaction descriptor is attached to RPC requests). Caveat: anything that
  statement text itself creates (`SET`, `USE`, a `#temp` table from `SELECT ... INTO`) ends with the call. That is why
  the routing check above sends every text that could hold such a statement (a `SET`/`USE`/`DECLARE`/`CREATE`/...
  after the DML, with or without `;`) to the plain batch, at the price that an unusual text is not counted. The check
  is conservative, not a parser. A `WITH ... SELECT` reports the number of rows it returns, which is harmless.
- **Kept: cancel closes the connection.** `tiberius` 0.13 does have `Client::cancel_query` (a TDS attention), so the
  earlier "no attention path" wording is corrected. It stays closed because an attention leaves an open transaction
  open, which would change the documented "a cancel rolls back" behaviour and needs its own design and tests. `KILL`
  from a second connection needs `ALTER ANY CONNECTION`, which a normal login lacks.
- **Kept: `money` and `smallmoney` as float.** `tiberius` decodes them to `f64` and does not expose the exact value.
  Workaround: `CAST(x AS decimal(19,4))`. A grid edit whose `WHERE` compares a `money` column can match 0 rows and then
  rolls back (nothing is written).
- **Kept: one result set per statement.** Later sets are dropped. `QueryEvent` has no message channel to say so per
  result, and changing the driver API is out of scope, so it is documented only. With the T-SQL splitting of
  [ADR-0073](0073-tsql-dialect-and-manifest-dialect.md) this only cuts a procedure or batch that returns several sets.
- **Tests.** Unit tests cover the routing, the count rule and the parameter declaration. Container tests
  (`db_mssql_dml.rs`) cover counts per statement and the `NOCOUNT` and trigger cases; `db_edit_mssql.rs` in `sqler-ui`
  applies staged edits end to end. A run against a real SQL Server instance is pending (maintainer).
