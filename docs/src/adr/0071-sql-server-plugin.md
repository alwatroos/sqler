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
  no T-SQL dialect (so the production gate confirms every statement), a host-driver plugin cannot supply catalog SQL,
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
