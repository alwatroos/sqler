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
sqlserver://user:password@host:1433/database
```

`mssql://` is accepted as an alias. The port defaults to 1433 and the database to the default of the
login. User, password and database may be percent-encoded. A query part (`?key=value`) is refused.
Windows authentication and Azure AD are not supported. Put the password in the target: the keychain
password and SSH tunnels are not used for this driver yet.

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
- **T-SQL dialect.** The plugin declares the dialect `tsql` ([ADR-0073](../../adr/0073-tsql-dialect-and-manifest-dialect.md)).
  A batch separator `GO` is recognised only when it is alone on its line (a trailing `--` comment is
  allowed); `GO 5` is not a separator and stays in the statement, so the server reports it. Identifiers
  are quoted as `[name]`, and parameters are bound as `@P1`, `@P2`, and so on (other `@x` names are
  your own variables).
- **Production confirmation.** On a production connection a plain read (`select`, also with `top`,
  `with (nolock)` or `option (..)`) runs without asking; every other statement asks for
  confirmation, twice. The check is deliberately strict: `exec` or `execute` anywhere, `kill`,
  `checkpoint`, `waitfor` (except a leading `waitfor delay`/`time`), `dbcc`, lock hints such as
  `updlock` or `holdlock`, `select .. into` and `begin` blocks all ask. A column or alias named
  `move`, `send`, `receive` or `enable` also asks unless it is written in brackets (`[move]`). A select
  that uses `apply`, `pivot`, `tablesample` or a table hint in the old form `from t (nolock)` asks too.
- **Export and AI tools.** Fresh-run export, the SQL `INSERT` export and the AI query tools work
  (`INSERT` statements hold at most 1000 rows each, text is written as `N'..'`). The AI sampling tool
  uses `select top (n)`; `explain` is not available on SQL Server, in the editor or for the AI.
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
