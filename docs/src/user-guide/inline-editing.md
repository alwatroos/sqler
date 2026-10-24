# Inline editing

A results grid is read-only unless SQLer can prove which table and which row each cell belongs to. When it can, you edit cells in place, see the pending changes marked, look at the exact SQL and apply everything in one transaction. Nothing is written until you apply. The rules are in [ADR-0030](../adr/0030-staged-edits-and-dml.md) and the interface in [ADR-0033](../adr/0033-inline-editing-ui.md).

The bar above the grid says **Editable**, or **Read-only** with the reason as a tooltip. Editing works on `SQLite`, `PostgreSQL`, `MySQL`/`MariaDB` and SQL Server (see [SQL Server](databases/sqlserver.md)).

## What can be edited

- The result of a plain `SELECT` from one table (without a join, `GROUP BY`, `DISTINCT` or a set operation) whose columns include every column of the table's primary key (SQLer does not read unique keys yet). A table opened from the object browser ("open data") qualifies too.
- A view, a join, a computed column and a result that lacks a key column are read-only. The reason is shown as a tooltip on **Read-only**.
- A table without a usable key can be edited after you choose **Edit without a key...** (command `grid:edit-without-key`, "Results: Edit Without a Key"). A dialog titled "Edit without a key?" warns that rows are then found by all their values, and an apply that changes zero or several rows is undone. The bar then shows "rows matched on all values".
- Binary cells (`bytea`, `blob`, `binary`) are read-only in this release. Hex editing is deferred.
- While rows are still arriving (before the result is complete or paused) or while an apply runs, the grid is not editable. A paused result can be edited.
- **The schema probe.** A query names its table without a schema, so SQLer asks the database which schema the name resolves to on the query's own session (a temporary table can shadow the table SQLer knows). After each single-table `SELECT` that could be edited, SQLer therefore runs one extra read-only statement (a catalog lookup with the table name as a bound parameter) on the same session. It reads no data of yours. If it fails, the grid is simply read-only.

## Edit cells

| How | What it does |
| --- | ------------ |
| Double-click a cell, Enter or F2 | Opens the cell editor (`grid:edit-cell`, "Results: Edit Cell"). The editor suits the column: a toggle for booleans, text for the rest, and numbers, dates and JSON are checked against the column type. |
| Enter, Tab, Shift+Tab | Keep the value (Tab and Shift+Tab also move to the next or previous cell). An invalid value stays open with a message. |
| Escape | Closes the editor without a change. |
| Delete (Cmd+Backspace on macOS) | Marks the selected rows for deletion (`grid:delete-rows`). |
| Palette commands | `grid:edit-cell`, `grid:set-null` ("Results: Set Cell to NULL", not for `NOT NULL` columns), `grid:revert-selection` ("Results: Revert Selected Changes"), `grid:add-row` ("Results: Add Row"), `grid:delete-rows`, `grid:preview-changes`, `grid:apply-changes`, `grid:discard-changes`, `grid:edit-without-key`. |

The edit bar has the buttons **Add row**, **Preview**, **Discard** and **Apply**.

Clicking elsewhere while the editor is open keeps a valid value and drops an invalid one with a notice. Edited cells, new rows and rows marked for deletion are drawn with their own marks (also in the dark theme), and the bar shows how many changes are pending. There is no undo stack: `grid:revert-selection` drops the changes of the selected cells and **Discard** drops all of them. Sorting and filtering use the values as they were read, and new rows stay at the end of the grid.

The commands have no default shortcut. `Mod+S` and `Mod+Shift+S` are deliberately not bound: a default shortcut is global in SQLer (it also works while you type in the SQL editor) and `Mod+S` means "save" in most places, so applying changes needs the palette (`grid:apply-changes`), the **Apply** button, or a shortcut you choose in `keybindings.toml` (see [Keybindings](keybindings.md)).

## Preview and apply

**Preview** opens a dialog titled "Pending changes". It lists every statement that will run, then each value apart from the SQL as `column (type) = value`, so you can check both. **Copy SQL** copies the list, **Close** leaves, and **Apply** runs the same list. Every value is sent as a bound parameter; it is never part of the statement text.

- **Production.** On a connection marked `prod`, **Apply** first opens a confirmation titled "Apply to production?" with every statement and value; Cancel has the focus, and a held key does nothing. Declining writes nothing. Connections of other environments, or with none, apply directly.
- **Which session.** A query result is applied on the query tab's own session, the one that read the rows, so temporary tables and the search path match what you see. A table opened from the browser applies on a short new session.
- **The transaction guard.** SQLer cannot apply inside your own transaction, because the changes would commit or lose it (and `MySQL` commits it implicitly). The query tab remembers whether you opened one with `BEGIN`, `START TRANSACTION`, `SAVEPOINT` or `SET autocommit = 0`, and Apply is refused with a notice until you `COMMIT` or `ROLLBACK`. After `SET autocommit = 0` on `MySQL` that is not enough, because the next transaction starts at once: run `SET autocommit = 1` (the flag stays set after `COMMIT` or `ROLLBACK`). SQLer errs toward "in a transaction": it cannot see a `COMMIT` inside a procedure, a function or a `DO` block, nor a `MySQL` implicit commit. If the guard is wrong, discard the pending changes first, then run `COMMIT` or `ROLLBACK` (or `SET autocommit = 1` with autocommit off). A table opened from the browser is never inside your transaction.
- **While it runs.** The bar shows "Applying N statements…" with **Cancel**. Cancel before the final `COMMIT` rolls everything back; the edits are kept.
- **After the apply.** On success the grid shows what was written, in place: it re-reads nothing, so a default, a trigger or a generated column is not shown until you run the query again. Rows changed or added by the apply are read-only until then. On failure the transaction is undone, the failing row is marked and your edits are kept.
- **Guards.** A query with pending changes is not run again ("Apply or discard the N pending change(s) first."), and closing the tab or quitting asks first in a dialog titled "Discard pending changes?" with the buttons **Keep open** and **Discard changes**. While an apply runs the tab cannot be closed.

## Limits

Things to know before you apply edits:

- The statements are shown before they run, with your values listed apart from the SQL. All changes run in one transaction and are undone if any statement does not change exactly one row.
- If the connection fails while the final `COMMIT` is being sent, SQLer cannot know whether the changes were saved. It keeps your edits and asks you to re-read the data before trying again, so you do not insert rows twice.
- A value that does not fit the column (a longer text for `varchar(5)`) is an error on PostgreSQL and on MySQL or MariaDB in strict `sql_mode`, and the whole apply is undone. SQLite does not enforce `varchar(n)`, so it stores the longer text in full. MySQL or MariaDB with a non-strict `sql_mode` cuts the value with a warning, and SQLer only checks that each statement changed one row.
- A `NaN` written to a `SQLite` `REAL` column is stored as `NULL`, and a time zone offset written to a `MySQL` `DATETIME` column is not kept; the database reports success for both.
- A `PostgreSQL` column whose type is a domain over `varchar(n)` or `char(n)` gives an error for a too-long value, as a plain column does. A domain over a non-text type or an array of enums fails with an error and a rollback, and a domain over `bit(n)` may cut a too-long value silently (a known gap).
- On `PostgreSQL`, a result that contains a `"char"` column or an `interval` column with fields (`interval year`, `interval day to second`) is read-only, because the database would change the value silently.
- A `SQLite` table with a generated or hidden column is read-only when the result contains that column (the table's column list does not include it).
- A keyless edit of a `SQLite` table with about a thousand columns fails with "Expression tree is too large (maximum depth 1000)" and is rolled back; nothing is written. Give the table a primary key, or edit with fewer columns in the result.
- On SQL Server an apply whose row count is unknown is rolled back and nothing is written; see [SQL Server](databases/sqlserver.md).
- If another user changes a row between your read and your apply, your edit is applied over it (the last writer wins), unless the key itself changed. The preview says so.

See also [Results grid](results-grid.md) and [Running queries](running-queries.md).
