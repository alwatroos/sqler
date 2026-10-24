# SQL Server

The `SQL Server` driver (id `sqlserver`) connects over TCP, with TLS, using a SQL login. It is a
plugin driver (see [Plugins](../plugins.md) and [ADR-0071](../../adr/0071-sql-server-plugin.md)). It
is in every 1.0 download, built in as a plugin.
It is tested against SQL Server 2022. A run against a real SQL Server instance is still pending.

## Set up

1. Open the **Plugins** view (the plug icon in the activity bar). "SQL Server" is listed; SQLer also opens the
   permission dialog "Allow plugin SQL Server?" the first time.
2. Approve the permission `driver.host:mssql` ("Use a built-in database engine"). Click **Approve** with the mouse
   or type the plugin id `sqlserver` in the confirmation field and press **Enter**. No key is bound to **Approve**;
   see [Answering](../plugins.md#answering).
3. Add a connection and choose the driver "SQL Server".

Until you approve, the driver is not offered. A `sqlserver` folder in `<config>/plugins` or under
`--plugin-dir` replaces the built-in one.

## Targets

```text
sqlserver://user@host:1433/database
```

`mssql://` is accepted as an alias. The port defaults to 1433 and the database to the default of the
login. User, password and database may be percent-encoded. `?user=name` also works: SQLer moves it
into the target before connecting. Any other query part (`?key=value`) is refused. Windows
authentication and Azure AD are not supported.

## Passwords and keychain

Keep the password out of the target: type it in the password field of the connection form and SQLer saves it
in the system keychain (see [Secrets](../secrets.md)). SQLer adds it (percent-encoded) to the target when it
connects. As for every driver, a saved target that holds a password is rejected (see
[Target rule](../connections.md#target-rule)).

## SSH tunnel

A connection can use an SSH tunnel (see [SSH tunnels](../ssh-tunnels.md)). The default port is 1433 when the
target has none. SQLer dials `127.0.0.1` on the tunnel's local port, and the name used for the TLS check is the
original host, so `verify-full` checks the certificate against that host. On Windows, `verify-full` through a
tunnel is refused before anything opens. A real check through an SSH tunnel against a real SQL Server
instance is still pending.

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

## Production confirmation

On a connection marked `prod`, a plain read (`select`, also with `top`, `with (nolock)` or `option (..)`)
runs without asking. Every other statement asks once: the "Run on production?" dialog lists the exact
statements of the run and Cancel is focused (see
[Production connections](../running-queries.md#production-connections)). The check is deliberately strict:
`exec` or `execute` anywhere, `kill`, `checkpoint`, `waitfor` (except a leading `waitfor delay`/`time`),
`dbcc`, lock hints such as `updlock` or `holdlock`, `select .. into` and `begin` blocks all ask. A column or
alias named `move`, `send`, `receive` or `enable` also asks unless it is written in brackets (`[move]`). A
select that uses `apply`, `pivot`, `tablesample` or a table hint in the old form `from t (nolock)` asks too.

The editor asks once. A statement that a plugin runs for you is different: you approve it in the plugin's own
dialog and then confirm it again on `prod` (see [Plugin writes](../plugins.md#plugin-writes)).

## Export

Export of the results works as for the other databases, including a fresh run
([Export and copy](../results-grid.md#export)). `INSERT` statements hold at most 1000 rows each, and text is
written as `N'..'`. The AI query tools work too; the AI sampling tool uses `select top (n)`.

## Inline editing

Inline editing in the results grid is applied on SQL Server ([Editing](../results-grid.md#editing)). The
number of rows changed by `insert`, `update`, `delete` and `merge` (and a statement that starts with `with`) is
shown. The count is unknown, and shown as such, for other statements, after `SET NOCOUNT ON`, when a trigger
counts its own statements (add `SET NOCOUNT ON` as its first statement), and for a batch or procedure with
several counts. When the count of an inline edit is unknown the apply is rolled back and nothing is written;
this is the safe default.

## Behaviour to know

- **Cancel closes the connection.** Cancelling a running statement ends the session; the server rolls
  back an open transaction. Run the next statement after reconnecting (the tab says "the SQL Server
  connection was closed by a cancel; connect again").
- **T-SQL dialect.** The plugin declares the dialect `tsql` ([ADR-0073](../../adr/0073-tsql-dialect-and-manifest-dialect.md)).
  A batch separator `GO` is recognised only when it is alone on its line (a trailing `--` comment is
  allowed); `GO 5` is not a separator and stays in the statement, so the server reports it. Identifiers
  are quoted as `[name]`, and parameters are bound as `@P1`, `@P2`, and so on (other `@x` names are
  your own variables).
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

## Limits

- SQL login only: no Windows authentication, no Azure AD.
- TLS: no `verify-ca`, no client certificates or keys.
- No Explain, in the editor or for the AI.
- Only the first result set of a statement is shown.
- `money` and `smallmoney` arrive as a float.
- The object browser covers the current database only.
- A cancel closes the session; connect again.
- A run against a real SQL Server instance (and through an SSH tunnel) is still pending.
