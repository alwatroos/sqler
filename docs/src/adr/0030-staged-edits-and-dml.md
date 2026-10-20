# 0030 Staged edits and DML: bound statements, single-table editability, one-transaction apply

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

The results grid will let the user edit cells, add rows and delete rows, preview the generated SQL and apply it (T11). Before any editor exists, the model behind it must decide whether a result may be edited at all, hold the pending changes, and turn them into the exact statements that run. Values come from the user and must never alter statement text (ADR-0024). The classifier is fail-safe (ADR-0020), identifiers are quoted per dialect (ADR-0015), the driver API has no transaction call (ADR-0004), and `sqler-ui` never depends on a driver crate (ADR-0016). AGENTS.md §8 requires an ADR for a new public API.

## Decision drivers

- Read-only by default; a write must be explicitly enabled and provably aimed at one row.
- No value text in SQL, on any of the three dialects.
- Nothing may run unlisted: the preview and the executed statements are the same list.
- A wrong row count must undo the whole apply, and the staged edits must survive a failure.
- Testable headless, on a real `SQLite` session.

## Considered options

1. Generate `?`-style SQL and reuse `query::bind` of ADR-0024.
2. Generate the dialect's native positional markers directly in a pure `sqler-sql::dml` module, and keep the edit model in `sqler_ui::grid::edit`.
3. Keep the edit model and the SQL generation together in `sqler-ui`.

## Decision outcome

Chosen: option 2. Option 1 re-lexes generated text, and its `PostgreSQL` text binding needs casts that `bind` cannot know. Option 3 would put SQL text in the UI crate and leave `sqler-sql` without a reusable generator. No new crate and no new dependency.

**Placement.** `sqler_sql::dml` (pure, next to `ddl.rs`) builds `Query` values from the catalog `Table` and column/value pairs. `sqler_ui::grid::edit` is built without the `gui` feature, so `cargo test -p sqler-ui` covers it. It holds editability, the staged edits, the apply plan with its preview, and the apply job. The view is T11's and lives behind `gui`.

**Editability rules.** A result is editable only when it comes from one base table and a key of that table is fully present in the result. Checked in this order; the first failure is the stated reason.

1. The connection has a `Dialect`, else `NoDialect`.
2. A table-data tab must show an object of kind `Table` (views and the like give `NotATable`).
3. A query result must pass `single_table_source`: one statement, `SELECT` of `*` or plain column names, `FROM` one table (optionally schema-qualified), then only `WHERE`, `ORDER BY`, `LIMIT`, `OFFSET` or `FETCH`. Joins, comma joins, `GROUP BY`, `HAVING`, `DISTINCT`, set operations, `WITH`, `INTO`, subqueries, aliases, qualified columns, expressions, executable comments and unterminated tokens all give `NotSingleTable`. So do unquoted niladic functions and pseudo-columns written without parentheses (`user`, `current_user`, `session_user`, `system_user`, `current_role`, `current_catalog`, `current_schema`, `current_date`, `current_time`, `current_timestamp`, `localtime`, `localtimestamp`, `utc_date`, `utc_time`, `utc_timestamp`, `sysdate`, `default`), which are values and not columns: `SELECT user, name FROM t` would otherwise show the session user under a column called `user`, and with that column as key every edit would be aimed at the wrong row. A quoted `"user"` is a real column and is accepted. The same words are refused as a bare `FROM` name (a `PostgreSQL` function scan), but not after a `.` (`mysql.user`). The analysis uses the ADR-0020 lexer and is conservative: when unsure it says no.
4. The table's columns must be loaded (`TableNotLoaded`) and its name and schema must agree with the analysed source (`TableMismatch`). A qualified `FROM` must name the loaded table's schema. An unqualified `FROM t` is resolved by the server (`search_path` and `pg_temp`, the current `MySQL` database, `SQLite` `temp` before `main`), which the model cannot know, so `EditRequest::resolved_schema` carries the schema the server resolves **that name as written in the query** to: the caller resolves it per query, on the session that ran it, right after it ran (`PostgreSQL` `to_regclass` of the name, which searches `pg_temp` first; `SQLite` the first of `temp`, `main` and the attached databases, in attach order, that holds the name; `MySQL` `DATABASE()`). A session-level value (`current_schema()`, a constant `main`) is wrong exactly when a temporary table shadows the loaded one, and the writes would then go to the table named in the plan, not the one the user sees. The result is editable only when `resolved_schema` equals the loaded table's schema exactly, and read-only when it differs or is `None`. Without this, `SELECT * FROM t` could read a `t` of another schema and the key-filtered writes would go to the wrong table. An unquoted `PostgreSQL` name is folded ASCII-only, as the server does for UTF-8.
5. Every result column must map to exactly one table column (`UnmappedColumn`, `DuplicateColumn`). On `SQLite` a result column that `table_info` does not list is reported as `HiddenColumn`: generated and hidden columns are returned by `SELECT *` but left out of `table_info`, so such a table is read-only, with a message that says so. A mapped column that cannot be written without a silent change gives `UnwritableType` (see the casts below: `"char"` and `interval` with fields on `PostgreSQL`). Matching follows the dialect: `PostgreSQL` exact on the catalog name, `SQLite` and `MySQL` ASCII case-insensitive; an unquoted identifier in the query is folded the way the server folds it.
6. Row identity: the primary key if all its columns are in the result, else the first caller-supplied unique key whose columns are all `NOT NULL` and present. The catalog has no unique keys yet (`Table` has only `primary_key`), so the list is empty in M4; the rule and the API are in place. No key gives `NoKey`; a key with columns missing from the result gives `KeyNotInResult`.

**Row identity.** `RowIdentity::Key` matches a row by the key columns with their original values. A row whose original key value is `NULL` cannot be edited (`NullKey`), because `= NULL` matches nothing. `RowIdentity::AllColumns` is used only in opt-in mode.

**Opt-in mode.** `NoKey` and `KeyNotInResult` can be overridden by an explicit user choice (`allow_keyless`). Rows are then matched on all original column values. Views, joins, aggregates and unmapped columns never opt in. The row-count check below is the safety net: two identical rows make an UPDATE or DELETE touch two rows, the check fails and the apply rolls back.

**Staged edits.** `StagedEdits` can only be built from an `EditTarget`, so read-only is enforced by the type: there is no way to stage a change for a result that did not pass `check_editable`. Changed cells and deletes are keyed by the model row, the row's position in `GridModel`; the original row is snapshotted when the first change is staged. Sort and filter only permute the view over the model, so staged edits survive them with no index change. Setting a cell back to its original value drops the change; deleting a row drops its cell changes; editing a deleted row is refused. New rows live in their own list with ids in creation order, and only cells the user filled are inserted. An empty new row is refused (`EmptyInsert`), there is no `DEFAULT VALUES`. The caller clears the staged edits after a commit and when the result is re-run (a re-run invalidates the row indices).

**Statement shape.** No trailing `;`, single spaces, identifiers quoted with `quote_identifier`/`qualified_name`, every value in `Query::params`, never a string literal.

```text
UPDATE <t> SET <c> = <p>, ... WHERE <c> = <p> AND ...
INSERT INTO <t> (<c>, ...) VALUES (<p>, ...)
DELETE FROM <t> WHERE <c> = <p> AND ...
```

A `NULL` in a filter renders `<c> IS NULL` and binds nothing; a `NULL` in `SET` or `VALUES` is bound. An empty filter is an error (`EmptyFilter`), so an unfiltered UPDATE or DELETE cannot be generated.

| Dialect | Marker | Values |
| ------- | ------ | ------ |
| `PostgreSQL` | `$k` | one per occurrence, in order of appearance |
| `SQLite` | `?k` | one per occurrence, same order |
| `MySQL`/`MariaDB` | `?` | one per occurrence, same order |

`k` is the 1-based order of appearance and `params` follow the same order.

**`PostgreSQL` casts.** The driver binds `Decimal`, `Date`, `Time`, `Timestamp`, `TimestampTz`, `Json` and `Text` as `text` and `Null` as `int8`, so a bare `$k` against a typed column fails. Those kinds are written as `CAST($k AS T)`, and `Null` as `CAST(CAST($k AS text) AS T)`; `Bool`, `Int`, `Float` and `Bytes` stay `$k`. `T` is the column's catalog `native_type` made safe, otherwise a type derived from the column's `data_type`, otherwise no cast.

- `T` carries **no length, precision or field modifier**. An explicit cast to `character varying(5)`, `character(3)`, `bit(4)` or `numeric(5,2)` silently truncates, pads or rounds, where plain assignment to the column raises "value too long"; a 10-character edit into a `varchar(5)` would commit as 5 characters. The generator strips every `(...)` (`timestamp(3) with time zone` becomes `timestamp with time zone`, `character varying(5)[]` becomes `character varying[]`) and the server's assignment check against the column's own modifier then applies: a value that does not fit is an error and the apply rolls back. `character(n)` becomes `bpchar` and `bit(n)` becomes `bit varying`, because `character` and `bit` alone mean length one. `interval` spellings are kept as written (`interval day to second(3)`, `interval year`): the modifier does not change how a value is truncated, and removing the fields would change what the server reads. They are still refused for writing, see below.
- A spelling without a quote is used only if it is 1 to 128 ASCII bytes of letters, digits, `_`, space, `,`, `.`, `(`, `)`, `[`, `]`, not blank, with balanced `()` and `[]` (a quote, backslash, `$`, `?`, `;`, comment or control character is never allowed, so the spelling cannot change how the placeholder scanner lexes the SQL). A spelling with a double quote (an enum or domain whose name needs quoting, `"Mixed Enum"`, `public."Mixed Enum"`) is accepted only as `ident[.ident]` with an optional `[]` suffix, ASCII, without single quote or control character, and is rebuilt part by part with `quote_identifier`, so what reaches the SQL is always an identifier the generator quoted itself. Anything else uses the logical type; the server then rejects an incompatible value and the apply rolls back.
- A `PostgreSQL` filter on a `json` column renders `CAST(<c> AS text) = $k`, because `json` has no `=`. A `MySQL` filter on a `JSON` column renders `` `c` = CAST(? AS JSON) ``, because a text parameter compares as a JSON string and never equals a document (a keyless edit of any `MySQL` row with a JSON value would never match). `MariaDB`'s `JSON` is a text column and needs no cast.
- **Domains.** Introspection reports a domain as its base type's logical type with the domain's name as `native_type` (`create domain d5 as varchar(5)` gives `Text` and `d5`). Casting to the domain by name would apply the base type's explicit-cast rules and truncate silently. So when the logical type is not `Other` and the spelling (without `[]`) is not a built-in type name, the parameter is cast to the logical type instead (`text`, `text[]`, `numeric`, ...), and the server's assignment rules apply on the column: a too-long value is an error and the apply rolls back. The trade-off: a domain over a non-string base (`uuid`, `inet`) and an array of enums (arrays are always `Text`) also fail, with a rollback and the edits kept, never a wrong write. A domain over `bit(n)` reports `Other`, cannot be told from an enum, and is still cast by its name, so a too-long value is cut silently; this is a documented gap.
- **Values the server would change silently, refused.** The one-byte type `"char"` keeps only the first byte of a longer text on assignment as well as on a cast, so it cannot be written safely. An `interval` with fields (`interval year`, `interval day to second(3)`) reads a bare `1` by its fields only when the server parses a literal; sent as a text parameter it is read as seconds and then masked, so `1` becomes `00:00:00`. `sqler_sql::unwritable_reason` names both; `update` and `insert` refuse them (`DmlError::UnwritableType`) and `check_editable` makes a result that contains such a column read-only (`UnwritableType`). `interval` without fields takes a bare number as seconds on the server as in the generator.

**Apply plan.** The plan is an ordered, fully listed set of statements: `BEGIN` (`START TRANSACTION` on `MySQL`), the body, `COMMIT`, and `ROLLBACK` for failures. The body order is DELETE (ascending row), UPDATE (ascending row, changed columns ascending), INSERT (creation order); deleting first frees keys before they are reused. Each filter uses the original values. Each body statement expects exactly one affected row. The preview lists the same `Query` values the job executes, with the SQL, the markers, and the bound values shown separately with their column and type. Values are shown as they are, user data is not redacted. The job executes only the plan's `begin`, `statements`, `commit` and `rollback`, so nothing can run unlisted; the prod confirmation (ADR-0024) is T11's and shows this list. The gate is a type: `apply` takes a `ConfirmedPlan`, which exists only through `ApplyPlan::confirm(production, user_confirmed)` (a `prod` plan the user did not confirm gives `ConfirmationRequired`) or `ApplyPlan::confirm_non_production()` for connections that are not `prod`. A caller cannot reach `apply` on a `prod` connection without the confirmation API, and the token is consumed by one `apply`, so a retry asks again. `needs_confirmation(production)` is the question T11 asks before it shows `render_preview`. The preview takes the parameter markers and the bound column of every value from `sqler_sql::dml` itself (`param_marker`, `update_bound`, `insert_bound`, `delete_bound`), so it cannot drift from the executed `Query::params`.

**Transaction and row counts.** There is no transaction call in the driver API, so the job runs `BEGIN`, the body, `COMMIT` as statements on one borrowed session. After each body statement it drains the stream and requires `rows_affected == Some(expected)`. A different count (0 or 2 rows), a driver error, or a cancel (checked before each statement and before `COMMIT`) runs `ROLLBACK`, which ignores the cancel flag, and reports `RolledBack` with the failing body index (`None` for a cancel seen before `COMMIT`) and any rollback error. A failing `BEGIN` reports `NotStarted` after a `ROLLBACK` whose result is ignored, because the server may have begun the transaction before the error reached the client (the `MySQL` driver reports a cancel for a completed statement). An error or cancel **returned by `COMMIT` itself** is reported as `CommitUnknown`, never as `RolledBack`: the connection may have dropped after the server committed, and the `MySQL` driver turns a statement that completed into `Cancelled` when the cancel flag was set while it ran. A `ROLLBACK` is still sent (harmless after a commit, needed if `COMMIT` failed and left the transaction open). The staged edits are kept, but the caller must not offer a plain retry: it tells the user to re-read the data first, because a retry could repeat `INSERT`s (duplicates on tables with generated keys). The job never touches `StagedEdits`, so on any failure the edits are kept. DML only, so the `MySQL` implicit commit of DDL is irrelevant. `MySQL` reports matched rows because the driver sets `CLIENT_FOUND_ROWS`, so an UPDATE that changes nothing does not roll back falsely.

**Precondition: no open user transaction.** The session passed to `apply` must not be inside a user transaction. The driver API exposes no transaction state, so `apply` cannot check this and the caller guarantees it (T11 applies on a session with no open transaction). Inside an open transaction `PostgreSQL` only warns on `BEGIN`, so the plan's `COMMIT` would commit, and a failed apply's `ROLLBACK` would discard, the user's pending work; `MySQL`'s `START TRANSACTION` implicitly commits it. This is stated in the rustdoc of `apply` and of the `edit` module.

**Blind spots.**

- `PostgreSQL` types without `=` (such as `xml` or `point`) or with odd casts fail at execute; the apply rolls back and the edits are kept.
- Keyless matching compares all result columns; it is a guard, not an identity. The one-row check is the only protection against duplicates, and a concurrent change between read and apply shows up as a row-count failure, not a silent overwrite.
- The grid shows what was read; a trigger or generated column can make the stored row differ from the staged value. The result is not refreshed after a commit by this model.
- The catalog lists no unique keys, so only primary keys qualify in M4.
- A query result is analysed by text; a view with the same name as the analysed table cannot be told from a table except through the catalog lookup.
- Matching is `ASCII` case-insensitive on `MySQL`, which is wrong for table names under `lower_case_table_names=0`; the server-resolved `resolved_schema` is compared exactly, a qualified name is not.
- Keyless (opt-in) matching can fail to find a row, and then the row-count check rolls the apply back: a `MySQL` `DECIMAL` compared with a text parameter compares as a double, so a high-precision `DECIMAL` key can match two rows (rollback); `SQLite` `DATETIME` text containing `T` is normalised on read, so it never matches on write-back (rollback).
- Engine behaviour the apply does not guard, shown here so the preview is not trusted blindly: `SQLite` stores `Value::Float(NaN)` in a `REAL` column as `NULL`, and `MySQL` stores a `TimestampTz` in a `DATETIME` column without its offset (the instant is converted by the server or the offset is dropped, depending on the column). Both report success.
- A `PostgreSQL` domain over `bit(n)` truncates silently (see the casts above); a domain over `varchar(n)` or `char(n)` errors and rolls back.
- A keyless edit of a `SQLite` table with about a thousand columns fails with "Expression tree is too large (maximum depth 1000)": the filter is an `AND` chain over all columns. It rolls back and nothing is written; a key edit of the same table works.
- Last writer wins: the filter matches the key (or the original values), not a version, so an edit of a row another user changed in the meantime is applied over it unless the key itself changed.

**Notes for T11.** N1: `resolved_schema` is trusted as given; pass the server's resolution of each query name on the query's own session (see rule 4), never the loaded table's own schema. The check runs at check time, so resolve right after the query ran: a temporary table created in between is not seen. N2: `CommitUnknown` is also returned for a definite server rejection at `COMMIT` (a deferred foreign key), which is the safe direction; word it as "re-read the data before retrying". `confirm_non_production()` is as strong as the caller's `prod` flag, so T11 derives the flag from the connection, never from a default. `ConfirmedPlan` is not `Clone`, so every apply, a retry included, needs a new `confirm`.

## Consequences

- Positive: values cannot alter statement text on any dialect; a proptest checks that no value text appears in the SQL.
- Positive: an unfiltered write cannot be generated, and a statement that hits other than one row undoes the whole apply.
- Positive: T11 only renders the plan and calls `apply`; `sqler-app` and the drivers did not change.
- Negative: a failed apply leaves the user to find the offending row from the failure index.
- Negative: after `CommitUnknown` the user must re-read before retrying.
- Negative: `PostgreSQL` casts depend on catalog type names, so exotic types may fail at execute.
- Negative: the apply holds the tab session for the whole transaction, like a query run (ADR-0024).
