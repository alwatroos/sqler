# Results grid

The results grid shows rows in a table that draws only the rows you can see, so a result with a million rows scrolls as smoothly as one with a hundred. The grid is a tab next to your connection tabs. Real query results arrive with the SQL editor in M3. Until then the grid shows a synthetic result so you can try scrolling.

## Open a grid

1. Open the command palette (Cmd+Shift+P on macOS, Ctrl+Shift+P on Linux and Windows).
2. Run `dev:open-synthetic-grid` ("Open Synthetic 1M-Row Grid").

The command needs a connection: it uses the connection of the active tab, or the connection selected in the sidebar when no tab is open. With neither, nothing happens. The grid tab is titled `Results: <connection name>`. It does not open a database session.

## What you see

- **Headers.** Each column header shows the column name and, in a muted smaller font, its type (`integer`, `text`, `float` and so on).
- **Cells.** NULL shows as `NULL`. Line breaks and tabs in a value are shown as spaces, and a value longer than 256 characters is cut and ends with `…`. Text that does not fit the column is cut with an ellipsis.
- **Status line.** Above the table: `Waiting for columns…`, then `Loading… 1,024 rows` while rows arrive, and `1,000,000 rows` when loading is done. A cancelled load shows `(loading cancelled)` and a failed one `(failed: <message>)`. Next to it, `Rows 1–25 of 1,000,000` follows the visible range.
- **Paused.** A result is loaded one page at a time. When a page is full and the query has more rows, the status line shows `100,000 rows loaded; more rows available` and a **Fetch more** button appears (in the grid toolbar, and next to Cancel in a query tab). It loads the next page. The same action is the palette command `grid:fetch-more` ("Fetch More Rows"); with nothing paused it shows "No more rows to fetch." A result that ends exactly at a page boundary shows its row count, not the paused text.
- **Top and End.** The buttons jump to the first and the last row. The same actions are the palette commands `grid:scroll-to-top` and `grid:scroll-to-end`, which act on the active grid tab. They have no default shortcut.

You can scroll while the rows are still loading. Loading runs on its own thread and never blocks the window.

## Columns

- **Resize.** Drag the right edge of a header. Double-click the edge to fit the column to its header and the rows on screen (`grid:autofit-column`). Widths stay between 40 and 1,200 pixels.
- **Move.** Drag a header to another place, or run `grid:move-column-left` or `grid:move-column-right` for the column of the active cell.
- **Pin.** `grid:pin-column` pins the column of the active cell at the left edge, so it stays in view while you scroll sideways; `grid:unpin-column` releases it. Pinned columns can only be moved among themselves, and the others among themselves.
- **Reset.** `grid:reset-layout` restores the original order, the default widths and no pinned columns.

These commands are in the palette and have no default shortcut. The layout belongs to the open table: it is not kept after you close the tab or restart SQLer, and it resets when a result brings a different number of columns.

## Selecting cells

Click a cell to select it; the selected cell has a border. Shift+click extends the selection to a range. Click a column header to select the whole column (Shift+click extends it). The active cell and the ends of a range follow their cells when you move a column; the range itself is the block between them as currently shown, so moving a column into or out of that block changes which cells it covers.

When the grid has the focus (click a cell, or run a grid command from the palette), these keys work. `Mod` is Cmd on macOS and Ctrl elsewhere. Holding Shift extends the selection from the cell where it started.

| Keys | Action |
| ---- | ------ |
| Arrow keys | Move the active cell |
| Home, End | First, last column of the row |
| Mod+Home, Mod+End | First cell, last cell |
| Page Up, Page Down | Move by a page of rows |
| Tab, Shift+Tab | Move right, left; at the last or first cell the key is not used by the grid, so Tab leaves the table |
| Mod+A | Select all cells |
| Shift+Space | Select the row of the active cell |
| Ctrl+Space | Select the column of the active cell (Ctrl on every system) |
| Shift+arrows, Shift+Home and so on | Extend the selection |

The view scrolls to keep the active cell visible. Ctrl+Space belongs to the grid only while a results table has the focus; in the SQL editor it still triggers completion. The same actions are palette commands (`grid:move-*`, `grid:extend-*`, `grid:select-all`, `grid:select-row`, `grid:select-column`); see [Keybindings](keybindings.md#results-grid).

## Close a grid

Closing the tab cancels the loading and frees the rows. A synthetic grid is not paged and holds all its rows, so close grids you no longer need; the memory it takes is on the [Benchmarks](../benchmarks.md) page.

## Pages and paused results

The page size is the `result_page_rows` setting (default 100,000 rows, range 1,000 to 10,000,000; see [Settings](settings.md)). A page also ends at 64 MiB of cell data, whichever comes first, so the grid can hold slightly more than one page (by at most the last row that reached the 64 MiB, for example one very large cell). While a result is paused the database is not read further and nothing more is stored.

- **Cancel while paused.** Cancel (`query:cancel`) ends the statement and keeps the rows already loaded.
- **Close while paused.** Closing the tab stops the loading and gives the session back.
- **Scripts.** A paused statement holds the tab's session, so the next statements of a script stay pending until you fetch the rest of that result or cancel. If you close the tab, they are not run.

Memory is gated on a result with 6 columns (the `grid_viewport` default); other widths cost more or less per row. Figures are on the [Benchmarks](../benchmarks.md) page.

## Not yet available

Sorting, filtering, copying cells and exporting arrive in M4. Editing is described below.

## Editing

A results grid is read-only unless SQLer can prove which table and which row each cell belongs to. When it can, you edit cells in place, see the pending changes marked, look at the exact SQL and apply everything in one transaction. Nothing is written until you apply. The rules are in [ADR-0030](../adr/0030-staged-edits-and-dml.md) and the interface in [ADR-0033](../adr/0033-inline-editing-ui.md).

### What can be edited

- The result of a plain `SELECT` from one table (without a join, `GROUP BY`, `DISTINCT` or a set operation) whose columns include every column of the table's primary key (SQLer does not read unique keys yet). A table opened from the object browser ("open data") qualifies too.
- A view, a join, a computed column and a result that lacks a key column are read-only. The reason is shown as a tooltip on the status line of the grid.
- A table without a usable key can be edited after you choose **Edit without a key**. SQLer warns that rows are then found by all their values, and an apply that changes zero or several rows is undone.
- Binary cells (`bytea`, `blob`, `binary`) are read-only in this release. Hex editing is deferred.
- While rows are still arriving (before the result is complete or paused) or while an apply runs, the grid is not editable. A paused result can be edited.
- **The schema probe.** A query names its table without a schema, so SQLer asks the database which schema the name resolves to on the query's own session (a temporary table can shadow the table SQLer knows). After each single-table `SELECT` that could be edited, SQLer therefore runs one extra read-only statement (a catalog lookup with the table name as a bound parameter) on the same session. It reads no data of yours. If it fails, the grid is simply read-only.

### Edit cells

| How | What it does |
| --- | ------------ |
| Double-click a cell, Enter or F2 | Opens the cell editor. The editor suits the column: a toggle for booleans, text for the rest, and numbers, dates and JSON are checked against the column type. |
| Enter, Tab, Shift+Tab | Keep the value (Tab and Shift+Tab also move to the next or previous cell). An invalid value stays open with a message. |
| Escape | Closes the editor without a change. |
| Delete (Cmd+Backspace on macOS) | Marks the selected rows for deletion. |
| Palette commands | `grid:edit-cell`, `grid:set-null` ("Set NULL", not for `NOT NULL` columns), `grid:revert-selection`, `grid:add-row`, `grid:delete-rows`, `grid:preview-changes`, `grid:apply-changes`, `grid:discard-changes`, `grid:edit-without-key`. |

Clicking elsewhere while the editor is open keeps a valid value and drops an invalid one with a notice. Edited cells, new rows and rows marked for deletion are drawn with their own marks (also in the dark theme), and the status line shows how many changes are pending. There is no undo stack: **Revert selection** drops the changes of the selected cells and **Discard** drops all of them. Sorting and filtering use the values as they were read, and new rows stay at the end of the grid.

The commands have no default shortcut. `Mod+S` and `Mod+Shift+S` are deliberately not bound: a default shortcut is global in SQLer (it also works while you type in the SQL editor) and `Mod+S` means "save" in most places, so applying changes needs the palette (`grid:apply-changes`), the Apply button, or a shortcut you choose in `keybindings.toml` (see [Keybindings](keybindings.md)).

### Preview and apply

**Preview** lists every statement that will run, then each value apart from the SQL as `column (type) = value`, so you can check both. **Apply** runs the same list. Every value is sent as a bound parameter; it is never part of the statement text.

- **Production.** On a connection marked `prod`, Apply first opens a confirmation titled "Apply to production?" with every statement and value; Cancel has the focus, and a held key does nothing. Declining writes nothing. Connections of other environments, or with none, apply directly.
- **Which session.** A query result is applied on the query tab's own session, the one that read the rows, so temporary tables and the search path match what you see. A table opened from the browser applies on a short new session.
- **The transaction guard.** SQLer cannot apply inside your own transaction, because the changes would commit or lose it (and `MySQL` commits it implicitly). The query tab remembers whether you opened one with `BEGIN`, `START TRANSACTION`, `SAVEPOINT` or `SET autocommit = 0`, and Apply is refused with a notice until you `COMMIT` or `ROLLBACK`. After `SET autocommit = 0` on `MySQL` that is not enough, because the next transaction starts at once: run `SET autocommit = 1` (the flag stays set after `COMMIT` or `ROLLBACK`). SQLer errs toward "in a transaction": it cannot see a `COMMIT` inside a procedure, a function or a `DO` block, nor a `MySQL` implicit commit. If the guard is wrong, discard the pending changes first, then run `COMMIT` or `ROLLBACK` (or `SET autocommit = 1` with autocommit off). A table opened from the browser is never inside your transaction.
- **While it runs.** The edit bar shows "Applying N statements…" with Cancel. Cancel before the final `COMMIT` rolls everything back; the edits are kept.
- **After the apply.** On success the grid shows what was written, in place: it re-reads nothing, so a default, a trigger or a generated column is not shown until you run the query again. Rows changed or added by the apply are read-only until then. On failure the transaction is undone, the failing row is marked and your edits are kept.
- **Guards.** A query with pending changes is not run again ("Apply or discard the N pending change(s) first."), and closing the tab or quitting asks first. While an apply runs the tab cannot be closed.

### Editing limits

Things to know before you apply edits:

- The statements are shown before they run, with your values listed apart from the SQL. All changes run in one transaction and are undone if any statement does not change exactly one row.
- If the connection fails while the final `COMMIT` is being sent, SQLer cannot know whether the changes were saved. It keeps your edits and asks you to re-read the data before trying again, so you do not insert rows twice.
- A value that does not fit the column (a longer text for `varchar(5)`) is an error on PostgreSQL and on MySQL or MariaDB in strict `sql_mode`, and the whole apply is undone. SQLite does not enforce `varchar(n)`, so it stores the longer text in full. MySQL or MariaDB with a non-strict `sql_mode` cuts the value with a warning, and SQLer only checks that each statement changed one row.
- A `NaN` written to a `SQLite` `REAL` column is stored as `NULL`, and a time zone offset written to a `MySQL` `DATETIME` column is not kept; the database reports success for both.
- A `PostgreSQL` column whose type is a domain over `varchar(n)` or `char(n)` gives an error for a too-long value, as a plain column does. A domain over a non-text type or an array of enums fails with an error and a rollback, and a domain over `bit(n)` may cut a too-long value silently (a known gap).
- On `PostgreSQL`, a result that contains a `"char"` column or an `interval` column with fields (`interval year`, `interval day to second`) is read-only, because the database would change the value silently.
- A `SQLite` table with a generated or hidden column is read-only when the result contains that column (the table's column list does not include it).
- A keyless edit of a `SQLite` table with about a thousand columns fails with "Expression tree is too large (maximum depth 1000)" and is rolled back; nothing is written. Give the table a primary key, or edit with fewer columns in the result.
- If another user changes a row between your read and your apply, your edit is applied over it (the last writer wins), unless the key itself changed. The preview says so.

## Measure it yourself

- `cargo run --release -p sqler-ui --example grid_scroll` scrolls 1,000,000 rows headless and prints, as `key=value` lines, the most rows needed in one frame and the data cost per frame. Flags: `--rows`, `--columns`, `--batch`, `--height`, `--row-height`, `--step`, `--overscan`.
- `SQLER_GRID_PROBE=1 sqler` opens the synthetic grid on the first saved connection, scrolls it in the real window and prints `SQLER grid_*` lines (frame intervals, rows drawn per frame, memory), then exits with 0 (pass), 1 (fail) or 2 (timeout after 120 s). It needs a saved connection.

The recorded numbers are on the [Benchmarks](../benchmarks.md) page.
