# SQL Server

The `SQL Server` driver (id `sqlserver`) connects over TCP, with TLS, using a SQL login. It is a
plugin driver (see [Plugins](../plugins.md) and [ADR-0071](../../adr/0071-sql-server-plugin.md)) and
is only in builds made with the feature `driver-mssql`; the prebuilt default does not include it.
It is tested against SQL Server 2022. A run against a real SQL Server instance is still pending.

## Set up

1. Build `sqler` (or `sqler-cli`) with `--features driver-mssql`.
2. Stage the plugin with `cargo run -p xtask -- plugins build` and copy
   `target/plugins/debug/sqlserver` (or `release`) to `<config>/plugins/sqlserver`.
3. Open the Plugins view and approve the permission `driver.host:mssql`. The CLI never prompts; it
   uses the stored approval.

Without the feature, the driver is listed as unavailable ("needs the `mssql` engine, which this
build does not include").

## Targets

```text
sqlserver://user@host:1433/database
```

`mssql://` is accepted as an alias. The port defaults to 1433 and the database to the default of the
login. User, password and database may be percent-encoded. `?user=name` also works: SQLer moves it
into the target before connecting. Any other query part (`?key=value`) is refused. Windows
authentication and Azure AD are not supported.

Keep the password out of the target: save it in the keychain with the connection, and SQLer adds it
(percent-encoded) when it connects. A password typed into the target still works.

## SSH tunnel

A connection can use an SSH tunnel. The default port is 1433 when the target has none. SQLer dials
`127.0.0.1` on the tunnel's local port, and the name used for the TLS check is the original host, so
`verify-full` checks the certificate against that host. On Windows, `verify-full` through a tunnel
is refused before anything opens. A real check through an SSH tunnel against a real SQL Server
instance is still pending (maintainer).

## TLS

The modes are those of [TLS modes](index.md#tls-modes); no setting means `prefer`.

| Mode | Encrypted | Certificate checked | Host name checked |
| ---- | --------- | ------------------- | ----------------- |
| `disable` | no (a server that forces encryption refuses) | no | no |
| `prefer` | yes | no | no |
| `require` | yes | no | no |
| `verify-ca` | not available | n/a | n/a |
| `verify-full` | yes | yes (CA file, else the OS roots) | yes |

`verify-full` works with a `server_name` set on the connection: the certificate is then checked
against that name instead of the host you dial. Client certificates and keys are not supported, and
`verify-ca` is refused with a message that points to `verify-full` or `require`.

## Behaviour to know

- **Cancel closes the connection.** Cancelling a running statement ends the session; the server rolls
  back an open transaction. Run the next statement after reconnecting (the tab says "the SQL Server
  connection was closed by a cancel; connect again").
- **Production confirmation.** There is no T-SQL dialect yet, so on a production connection every
  statement asks for confirmation, including a plain `select`.
- **Rows affected.** The number of rows changed by `insert`, `update` and `delete` is not shown. For the same reason, inline editing in the results grid cannot be applied on SQL Server until the driver reports a changed-row count: the apply is rolled back and nothing is written.
- **Result sets.** Only the first result set of a batch is shown.
- **Types.** `money` arrives as a float; `uniqueidentifier` and `xml` as text; `decimal` keeps its
  scale.
- **Parameters.** A statement with parameters runs through `sp_executesql`, so a `USE` or `SET` in
  it does not last; a statement without parameters keeps the session state.

## Object browser

The browser lists the current database only: schemas, tables, views, functions, procedures,
triggers, sequences, types, indexes and roles. Materialized views and extensions do not exist in
SQL Server. Databases are listed but not browsed across.
