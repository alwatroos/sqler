# SQL Server

The `SQL Server` driver (id `sqlserver`) connects over TCP, with TLS, using a SQL login. It is a
plugin driver (see [Plugins](../plugins.md) and [ADR-0071](../../adr/0071-sql-server-plugin.md)). It
is in every 1.0 download, built in as a plugin.
It is tested against SQL Server 2022. A run against a real SQL Server instance is still pending.

## Set up

1. Open the Plugins view: "SQL Server" is listed.
2. Approve the permission `driver.host:mssql`.
3. Add a connection.

A `sqlserver` folder in `<config>/plugins` or under `--plugin-dir` replaces the built-in one.

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
- **Rows affected.** The number of rows changed by `insert`, `update`, `delete` and `merge` (and a
  statement that starts with `with`) is shown, and inline editing in the results grid is applied on
  SQL Server. The count is unknown, and shown as such, for other statements, after `SET NOCOUNT ON`,
  when a trigger counts its own statements (add `SET NOCOUNT ON` as its first statement), and for a
  batch or procedure with several counts. When the count of an inline edit is unknown the apply is
  rolled back and nothing is written; this is the safe default.
- **Result sets.** Only the first result set of a statement is shown; later ones are dropped, and the
  result does not say so. The driver reports one result per statement. A script split at `;` or `GO`
  loses nothing; only a procedure or batch that returns several sets is cut.
- **Types.** `money` and `smallmoney` arrive as a float, because the driver library decodes them that
  way and the exact value is not available. Use `CAST(x AS decimal(19,4))` for an exact value. An
  inline edit whose row match compares a `money` column can match no row and is then rolled back.
  `uniqueidentifier` and `xml` arrive as text; `decimal` keeps its scale.
- **Parameters.** A statement with parameters runs through `sp_executesql`, so a `USE` or `SET` in
  it does not last. A text that is clearly one DML statement (`insert`, `update` but not
  `update statistics`, `delete`, `merge`, `with`) without parameters also runs through `sp_executesql`,
  to read its row count. If such a text also holds a later `set`, `use`, `declare`, `create`, `exec`,
  `select ... into #tmp` or similar, or a `;` followed by more text, it runs as a plain batch instead,
  keeps its session state and shows an unknown row count. Any other statement without parameters keeps
  the session state. A `with ... select` shows the number of rows it returns.

## Object browser

The browser lists the current database only: schemas, tables, views, functions, procedures,
triggers, sequences, types, indexes and roles. Materialized views and extensions do not exist in
SQL Server. Databases are listed but not browsed across.
