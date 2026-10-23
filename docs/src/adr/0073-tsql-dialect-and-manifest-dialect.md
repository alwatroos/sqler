# ADR-0073: T-SQL dialect and the manifest `dialect` key

- Status: accepted
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

SQL Server is a plugin driver ([ADR-0071](0071-sql-server-plugin.md)). `Dialect::from_driver_id("sqlserver")` was
`None`, and the manifest `dialect` key of `[[drivers]]` had no consumer ([ADR-0063](0063-plugin-or-core-per-engine.md),
gap G1). Without a dialect nothing is split, classified or given placeholders, so a `prod` connection asked for
confirmation of every statement (even `select 1`), and the fresh-run export, the SQL `INSERT` export and the AI query
tools refused the driver. The gate must stay fail-safe: only `Read` runs without asking, and `Read` has to be proved.

## Decision Drivers

- The production gate and the AI approval must never run a write as a read. A miss is a safety bug.
- `sqler-sql` stays free of driver ids beyond the built-in ones; the dialect of a plugin driver is data the plugin
  declares.
- No global mutable state, no change to the `Connector` trait or to `sqler-app/src/connect/**`.
- Every existing caller keeps its behaviour for the three existing dialects.

## Considered Options

- A process-wide dialect registry that plugins fill: rejected (global mutable state, test isolation).
- A new `Connector` method that returns the dialect: rejected (the connect code is frozen and every fake would change).
- A dialect that is plain data carried to each layer: chosen.

## Decision Outcome

### The dialect

`sqler-sql` gets `Dialect::TSql` (name `tsql`). Every T-SQL decision lives in the existing modules: lexer, splitter,
classifier, literals and quoting, DML, DDL, formatter and placeholders.

- **Names.** `Dialect::from_name` maps a manifest name to a dialect by exact, case-sensitive match on `Dialect::as_str`:
  `postgres`, `mysql`, `sqlite`, `tsql`. An unknown or absent name means no dialect, with no manifest error.
  `Dialect::from_driver_id` still knows only the built-in ids; it learns no `sqlserver`.
- **Quoting and literals.** `[name]` with `]` doubled. Text is `N'..'`; text with a NUL, or a backslash right before a
  line break (T-SQL line continuation), becomes `CAST(0x.. AS NVARCHAR(MAX))` from UTF-16LE. Bytes are `0x..`,
  booleans `1`/`0`, NaN and the infinities `NULL`, timestamps use `T`.
- **Placeholders.** Only `@P<k>` is a placeholder; any other `@x` is a variable. Transactions are
  `BEGIN TRANSACTION`, `COMMIT TRANSACTION`, `ROLLBACK TRANSACTION`. Browser SELECT is `SELECT TOP (n) *`. Create and
  drop templates are not offered.
- **Splitting.** `;` and a `GO` that is alone on its line (spaces, tabs and a `--` comment allowed) separate
  statements. `GO <n>` is not a separator: it stays in the statement and the server reports it, so a batch never runs
  once instead of `n` times. `BEGIN..END` and `CASE` blocks and routine bodies (`CREATE [OR ALTER]
  PROC|FUNCTION|TRIGGER`) are kept whole; `BEGIN TRAN` is not a block. An unclosed block merges the rest of the text,
  which only ever means fewer splits.
- **Lexer.** `[..]` and `".."` are quoted identifiers, `N'..'` a string, block comments nest, `#t` and `##t` are words,
  `@x`, `@@x` and `$5` are operands, `::` is an operator. Backtick and `?` are not T-SQL.

### Classifier rules

The scan is fail-safe: a statement is `Read` only when every word is proven harmless. T-SQL has no statement
terminator requirement, so `select 1 kill 52` is one text with two statements and no `;`. Because of that, the T-SQL
scan looks for these words anywhere in a statement, not only at the lead:

- `Unknown`: `EXEC` and `EXECUTE` anywhere (including `INSERT .. EXEC`, `GRANT EXECUTE`, `sp_executesql`), `OPEN`,
  `SETUSER`, `REVERT`, and any `BEGIN` that is not `BEGIN TRAN`, `TRANSACTION` or `DISTRIBUTED` (a block, `TRY`).
- `Write`: `KILL`, `CHECKPOINT`, `BULK`, `DBCC`, `BACKUP`, `RESTORE`, `RECONFIGURE`, `SHUTDOWN`, `RECEIVE`, `SEND`,
  `MOVE`, `UPDATETEXT`, `WRITETEXT`, `END CONVERSATION`, a `WAITFOR` that is not the lead (a lead `WAITFOR DELAY|TIME`
  is `Other`), the lock hints `UPDLOCK`, `XLOCK`, `HOLDLOCK`, `TABLOCK`, `TABLOCKX`, `SERIALIZABLE`, `REPEATABLEREAD`,
  and `SELECT .. INTO`.
- `Ddl`: `DENY`, `ENABLE`, `DISABLE`, `ADD`.
- `Other` (after the scan): `DECLARE`, `PRINT`, `SAVE`, `USE`, `SET`, `COMMIT`, `ROLLBACK`, `BEGIN TRAN`.
- `TOP (` after `SELECT`, `DISTINCT` or `ALL`, and `OPTION (` after an operand, are syntax and not calls. A small
  allowlist of T-SQL functions (`getdate`, `isnull`, `len`, `convert`, `newid`, `object_id`, ...) is not a call that
  makes a statement `Unknown`. A qualified call, a quoted call, an unknown function and `NEXT VALUE FOR` keep their class.
- Write and Ddl both ask for approval; only the label differs.

This is stricter than a lead-word rule on purpose (the direction is always more non-`Read`, never less). The cost: a
column or alias with an unquoted name `move`, `send`, `receive`, `enable` and the like asks for approval until it is
written as `[move]`. Likewise `APPLY`, `PIVOT`, `TABLESAMPLE` and `FROM t (nolock)` are calls to the scan, so a select
that uses them is `Unknown` and asks. Both are the safe side.

### Carrying the dialect (the seam)

The dialect is an `Option<Dialect>` field that defaults to `Dialect::from_driver_id`, so every existing caller is
unchanged:

- `ConnectionInfo::dialect` (AI tools), `PluginConnection::dialect` (plugin host), `PluginWriteRequest::dialect`
  (approval dialog) and `open_prod_gate(gate, dialect, sql)`.
- `PluginDrivers::dialect` (plugin host) is the first claim of the driver id whose `check()` is Ok, through
  `from_name`. The CLI (`export`, `mcp`) and the app (`AppData`, `SurfaceData`, the approver) use it.
- `Workspace::dialect_of` (UI) reads the manifests already in the plugin snapshot. If the manifests that declare the
  driver id disagree, or none declares a dialect, it gives `None`. The host picks the first claim, which is the only one
  that can be available.

A third-party plugin that declares `postgres`, `mysql`, `sqlite` or `tsql` now gets that dialect. This is intended: the
name is a promise by the plugin author that the engine speaks it, and a wrong promise can only make the gate looser for
that plugin's own connections, as a wrong built-in driver would. A plugin with an unknown name keeps today's behaviour
(every statement asks).

### Behaviour for `sqlserver`

- Production gate: `select 1` starts without asking, every other class asks (twice on `prod`).
- AI query tools and fresh-run export work; `sample_rows` uses `SELECT TOP (n)`; `explain_query` and the UI explain stay
  unavailable; SQL `INSERT` export is capped at 1000 rows per statement.
- Completion has a T-SQL flavour (quoting, `N'..'`, `#t`, variables, `dbo` default schema) with the common keyword,
  function and snippet lists only.
- Editing is allowed by the dialect (`NoDialect` no longer applies). The apply still rolls back, because
  `rows_affected` is `None`, until S4 reports the changed-row count.

## Consequences

- Gap G1 of [ADR-0063](0063-plugin-or-core-per-engine.md) and the dialect gap of [ADR-0071](0071-sql-server-plugin.md)
  are fixed by this ADR. `DuckDB` still has no dialect (no manifest name for it).
- The scan can only err towards asking. The hostile table, the property tests and the case tables of the three older
  dialects are unchanged except for additions.
- Inline grid edits on `sqlserver` still roll back until S4.
- No run against a real SQL Server instance was done for this change; it is pending (maintainer). The headless tests
  read the shipped `plugins/sqlserver` manifest; the container test needs `SQLER_TEST_MSSQL_URL`.
