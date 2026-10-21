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

## Copy

Copy the selected cells (or a row, a column, or everything with Mod+A) to the clipboard. The text follows what the grid shows: the column order and pinned columns as you arranged them, and the row order of the grid. Mod+C works while the grid has the focus; every copy is also in the palette, and in the menu of a right click on a row. A right click does not change the selection.

| Command | Menu | Result |
| ------- | ---- | ------ |
| `grid:copy` (Mod+C) | Copy | Tab-separated values, no header |
| `grid:copy-with-headers` (Mod+Shift+C) | Copy with Headers | The same, with the column names on the first line |
| `grid:copy-as-markdown` | Copy as Markdown | A Markdown table |
| `grid:copy-as-json` | Copy as JSON | An array of objects, one per row |
| `grid:copy-as-insert` | Copy as SQL INSERT | `INSERT` statements, 100 rows each |

The copy uses the same writers as file export. In TSV, a value with a tab, a line break or a double quote is wrapped in double quotes (quotes doubled), so spreadsheets paste it into one cell; a single cell is copied without a trailing line break. NULL is copied as `NULL` in TSV, Markdown and `INSERT`, and as `null` in JSON; binary data is copied as hexadecimal text such as `0x00ff` in TSV and Markdown (`INSERT` and JSON use their own notation).

- **Empty strings and the text `NULL`.** NULL is copied as `NULL`, so a text value `NULL` is wrapped in quotes (`"NULL"`) to tell them apart; a text editor shows those quotes, a spreadsheet does not. When one column is copied and some values are empty strings, those rows are blank lines, and a single empty string is an empty clipboard; some paste targets drop blank lines, so copy a second column with them or use JSON.
- **`INSERT`.** It uses the SQL dialect of the tab's connection. A grid opened with "Open data" on a table or view uses that table's real (qualified) name; any other result, such as a query result, uses the placeholder `table_name`, which you edit after pasting. SQLer does not ask for a name. A connection without a SQL dialect (SQL Server, DuckDB) gets PostgreSQL quoting, and SQLer says so.
- **Rows fetched so far.** A copy takes the rows that are loaded. If the result is still loading or paused and your selection reaches the last loaded row, the notification says "Only the rows fetched so far were copied." Fetch more rows first if you need them all.
- **Limits.** A copy holds at most 1,000,000 cells and 50 MiB of text. Over a limit nothing is copied and a message says to select fewer rows or columns.
- **Large copies.** A large selection is read in slices and rendered in the background, so the window stays responsive; a copy of 100,000 cells or more, a partial copy and an `INSERT` with an assumed dialect show a notification when done. Starting another copy replaces the one in progress.
- **Formulas.** The text is copied exactly as stored. A spreadsheet may run a pasted value that starts with `=`, `+`, `-` or `@` as a formula, so paste data you do not trust into a text editor first.

The copied text is only put on the clipboard; SQLer does not log it or show it in notifications. The SQL editor keeps its own Mod+C.

## Close a grid

Closing the tab cancels the loading and frees the rows. A synthetic grid is not paged and holds all its rows, so close grids you no longer need; the memory it takes is on the [Benchmarks](../benchmarks.md) page.

## Pages and paused results

The page size is the `result_page_rows` setting (default 100,000 rows, range 1,000 to 10,000,000; see [Settings](settings.md)). A page also ends at 64 MiB of cell data, whichever comes first, so the grid can hold slightly more than one page (by at most the last row that reached the 64 MiB, for example one very large cell). While a result is paused the database is not read further and nothing more is stored.

- **Cancel while paused.** Cancel (`query:cancel`) ends the statement and keeps the rows already loaded.
- **Close while paused.** Closing the tab stops the loading and gives the session back.
- **Scripts.** A paused statement holds the tab's session, so the next statements of a script stay pending until you fetch the rest of that result or cancel. If you close the tab, they are not run.

Memory is gated on a result with 6 columns (the `grid_viewport` default); other widths cost more or less per row. Figures are on the [Benchmarks](../benchmarks.md) page.

## Not yet available

Sorting, filtering, editing cells and exporting arrive in M4.

## Editing limits

Editing is read-only unless the grid can prove which table and which row each cell belongs to; the edit view arrives in M4 and the rules are in [ADR-0030](../adr/0030-staged-edits-and-dml.md). Things to know before you apply edits:

- The statements are shown before they run, with your values listed apart from the SQL. All changes run in one transaction and are undone if any statement does not change exactly one row.
- If the connection fails while the final `COMMIT` is being sent, SQLer cannot know whether the changes were saved. It keeps your edits and asks you to re-read the data before trying again, so you do not insert rows twice.
- A value that does not fit the column (a longer text for `varchar(5)`) is an error on PostgreSQL and on MySQL or MariaDB in strict `sql_mode`, and the whole apply is undone. SQLite does not enforce `varchar(n)`, so it stores the longer text in full. MySQL or MariaDB with a non-strict `sql_mode` cuts the value with a warning, and SQLer only checks that each statement changed one row.
- A `NaN` written to a `SQLite` `REAL` column is stored as `NULL`, and a time zone offset written to a `MySQL` `DATETIME` column is not kept; the database reports success for both.
- A `PostgreSQL` column whose type is a domain over `varchar(n)` or `char(n)` gives an error for a too-long value, as a plain column does. A domain over a non-text type or an array of enums fails with an error and a rollback, and a domain over `bit(n)` may cut a too-long value silently (a known gap).
- On `PostgreSQL`, a result that contains a `"char"` column or an `interval` column with fields (`interval year`, `interval day to second`) is read-only, because the database would change the value silently.
- A `SQLite` table with a generated or hidden column is read-only when the result contains that column (the table's column list does not include it).
- A keyless edit of a `SQLite` table with about a thousand columns fails with "Expression tree is too large (maximum depth 1000)" and is rolled back; nothing is written. Give the table a primary key, or edit with fewer columns in the result.
- If another user changes a row between your read and your apply, your edit is applied over it (the last writer wins), unless the key itself changed.

## Measure it yourself

- `cargo run --release -p sqler-ui --example grid_scroll` scrolls 1,000,000 rows headless and prints, as `key=value` lines, the most rows needed in one frame and the data cost per frame. Flags: `--rows`, `--columns`, `--batch`, `--height`, `--row-height`, `--step`, `--overscan`.
- `SQLER_GRID_PROBE=1 sqler` opens the synthetic grid on the first saved connection, scrolls it in the real window and prints `SQLER grid_*` lines (frame intervals, rows drawn per frame, memory), then exits with 0 (pass), 1 (fail) or 2 (timeout after 120 s). It needs a saved connection.

The recorded numbers are on the [Benchmarks](../benchmarks.md) page.
