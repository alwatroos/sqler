# Results grid

The results grid shows rows in a table that draws only the rows you can see, so a result with a million rows scrolls as smoothly as one with a hundred. The grid is a tab next to your connection tabs. Real query results arrive with the SQL editor in M3. Until then the grid shows a synthetic result so you can try scrolling.

## Open a grid

1. Open the command palette (Cmd+Shift+P on macOS, Ctrl+Shift+P on Linux and Windows).
2. Run `dev:open-synthetic-grid` ("Open Synthetic 1M-Row Grid").

The command needs a connection: it uses the connection of the active tab, or the connection selected in the sidebar when no tab is open. With neither, nothing happens. The grid tab is titled `Results: <connection name>`. It does not open a database session.

## What you see

- **Headers.** Each column header shows the column name and, in a muted smaller font, its type (`integer`, `text`, `float` and so on).
- **Cells.** NULL shows as `NULL`. Line breaks and tabs in a value are shown as spaces, and a value longer than 256 characters is cut and ends with `…`. Text that does not fit the column is cut with an ellipsis. To read a whole value, use the [cell detail](#cell-detail) pane.
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

## Cell detail

Run `grid:toggle-cell-detail` ("Results: Toggle Cell Detail") from the palette to open a pane to the right of the table that shows the active cell in full. Run it again to close it. The pane is open or closed for the whole session (it starts closed and is not remembered after a restart), also across tabs and query reruns. Drag its left edge to resize it (240 to 1,200 pixels, 360 by default). The command has no default shortcut: the obvious chord, Mod+Shift+I, is already taken by the GPUI inspector (Ctrl+Shift+I) on Linux and Windows. Bind it in `keybindings.toml` if you want one (see [Keybindings](keybindings.md)).

The pane shows the column name, its type, the size of the value in bytes, and the value:

- **NULL** is shown as `NULL`. A text of zero length says `Empty text (not NULL).`
- **Text** is shown as stored, with its line breaks, split at every `\n` (a `\r` before it is dropped). Tabs are shown as spaces up to the next multiple of 4 columns, other control characters as their symbol (for example `␀` or `␍`), and the characters that switch the text direction as `�`, so a value cannot rearrange what you read. A line longer than 1,000 columns (a tab counts as the columns it takes) continues on the next row, marked `↪`.
- **JSON** is indented with two spaces. Key order, number text and escapes are kept as stored. JSON that is not valid, or nested at depth 128 or deeper, is shown as stored with a note. A JSON document over 8 MiB is never indented (the note says `Too large to format; shown as stored.`). Only JSON-typed columns are formatted: a text column that holds JSON is shown as text.
- **Binary** is a hex dump, 16 bytes a row, with the offset and an ASCII column.
- **Numbers, booleans, dates and times** are shown as the grid shows them.

Nothing in a value is interpreted: there are no links and no markup.

**Large values.** The pane shows at most 1 MiB of a text, a JSON document (before or after indenting) or a binary value, and says so in a note (`Showing the first 1.0 MiB of 10.0 MiB.`). A JSON document over 1 MiB is shown as stored, not indented, until you use Show all; over 8 MiB it stays as stored (it is still split into lines of at most 1,000 columns and scrolled the same way). **Show all** loads the rest in the background, so the window keeps responding; it keeps working on the value you selected, and selecting another cell cancels the wait. While a value is shown in full, SQLer holds the value, a copy of it for the background job and the lines of the pane. Measured with 50 MB values: a 50 MB text takes about 60 MB more, a 50 MB binary about 55 MB more, and a worst case of 52 million one-character lines about 260 MB more (a 4-byte offset per line). Formatted JSON in full is cut at 64 MiB.

**Copy value** puts the full value on the clipboard as plain text, whatever the pane shows: the stored text (also JSON, as stored, not indented), `0x` followed by the hex digits for binary, and a scalar as the grid shows it. The button is disabled for NULL. Copying a binary value builds a text twice its size in hex (about 30 ms for 50 MB).

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

## Sort

Sorting works on the rows the grid holds. Every header has a small sort indicator at its right edge (`↕` while the column is unsorted). Click the indicator to sort the column ascending (`↑`), click again for descending (`↓`), and again to remove the sort. Shift+click the indicator of another column to add it as a secondary key; with two or more keys the indicators are numbered (`↑1`, `↓2`). A click on the rest of the header still selects the column.

The commands `grid:sort-ascending` ("Results: Sort Ascending") and `grid:sort-descending` sort by the column of the active cell and replace the current sort; `grid:clear-sort` removes it. They have no default shortcut, and without an active cell they do nothing.

- **Order.** Values compare by their type: numbers by value, dates and times by instant, text by characters, and so on. NULL comes last in both directions. Rows that compare equal keep their original order.
- **Match case.** Text sorts and filters ignoring case by default. The **Match case** box in the filter bar (`grid:toggle-match-case`, "Results: Match Case in Sort and Filter") switches to comparing by Unicode code point. It is kept for the open table only.
- **Large results.** The sort runs in short slices on the interface thread, so the window stays usable; the previous order stays on screen until the new one is ready, and the status shows `Sorting… 40%`. Changing the sort or the filter while it runs starts over.

## Filter

The filter bar above the table has a **quick filter** (rows where any column, or the column picked in the list next to it, contains the text; applied shortly after you stop typing) and a **condition editor**: pick a column, an operator (`=`, `≠`, contains, `>`, `<`, is NULL, is not NULL), type a value and click **Add**. Each condition shows as a chip; click `×` on it to remove it. All parts combine with AND. NULL only matches "is NULL"; comparing with `≠` does not match NULL either. **Clear filter** (`grid:clear-filter`, "Results: Clear Filter") removes everything.

The status next to the bar shows `1,204 of 1,000,000 rows` while a filter is on, or `Sorted 1,000,000 rows` when only a sort is on.

## Loaded rows only

Sort and filter cover the rows the grid holds. While a result is still loading, or paused after a page (see [Pages and paused results](#pages-and-paused-results)), the status ends with `· loaded rows only`. Rows that arrive while a sort or filter is on are merged in once enough of them are there (or half a second after the last merge, or when loading ends), so rows can move under the view while a result loads. Fetching more rows extends the sorted and filtered view in the same way. Sorting by the database (`ORDER BY`) is not part of the grid.

**Known limit.** A MySQL text column with a binary collation (for example `VARCHAR ... COLLATE utf8mb4_bin`) reaches the grid as binary data, not as text. It sorts correctly (byte order), but the quick filter and the "contains" condition find nothing in it. This is a driver limit, not a grid one.

## Close a grid

Closing the tab cancels the loading and frees the rows. A synthetic grid is not paged and holds all its rows, so close grids you no longer need; the memory it takes is on the [Benchmarks](../benchmarks.md) page.

## Pages and paused results

The page size is the `result_page_rows` setting (default 100,000 rows, range 1,000 to 10,000,000; see [Settings](settings.md)). A page also ends at 64 MiB of cell data, whichever comes first, so the grid can hold slightly more than one page (by at most the last row that reached the 64 MiB, for example one very large cell). While a result is paused the database is not read further and nothing more is stored.

- **Cancel while paused.** Cancel (`query:cancel`) ends the statement and keeps the rows already loaded.
- **Close while paused.** Closing the tab stops the loading and gives the session back.
- **Scripts.** A paused statement holds the tab's session, so the next statements of a script stay pending until you fetch the rest of that result or cancel. If you close the tab, they are not run.

Memory is gated on a result with 6 columns (the `grid_viewport` default); other widths cost more or less per row. Figures are on the [Benchmarks](../benchmarks.md) page.

## Not yet available

Editing cells arrives in M4.

## Export

**Export Results...** (`grid:export`, in the command palette) writes the result of the active grid or query result to a file. It works on a table-data tab and on every result of a query tab. The dialog offers:

- **Format**: CSV, JSON, SQL `INSERT` and Markdown in every build. Excel (`.xlsx`) and Parquet are listed only when the build has them (the `export-xlsx` and `export-parquet` features), see below.
- **Scope**: *Loaded rows (N)*, the rows the grid holds now, as displayed (with the current sort and filter, in the order you see them; N is the number of rows shown), *Selection (rows x columns)* when cells are selected, copied in the order you see them, and *All rows (runs the query again)*.
- **Options**: for CSV the delimiter (`,` `;` Tab `|`), a header row and the NULL text; for Markdown the NULL text; for SQL the table name, an optional schema and the rows per `INSERT` (1 to 10,000). JSON, Parquet and Excel use their defaults.

After **Export...** the system save dialog asks for the file. A progress line with a **Cancel** button (also `export:cancel`) is shown while the file is written. The file is written under a hidden name next to the target and renamed when the export is complete, so a cancelled or failed export leaves no half-written file and an existing file is replaced only by a complete one. Replacing a file keeps its permissions on Unix; a symbolic link at the target is replaced by a regular file and the file it pointed to is left alone. If the result changes while the dialog is open (the query runs again, the table is opened again), the export is cancelled with a notice. Closing the tab or quitting the app cancels its export.

Things to know:

- *Loaded rows* exports what the grid shows: when a sort or filter is set, only the matching rows, in the sorted order, exactly as copy and the selection export do. *All rows (runs the query again)* ignores the grid's sort and filter.
- *Loaded rows* covers what the grid holds. A result that is paused at its page limit (see "Pages and paused results") exports only the loaded pages; the dialog shows the count.
- *All rows (runs the query again)* runs the statement once more on a new, separate session and streams every row into the file, without loading them into the grid. The data can differ from what the grid shows, because it is read again now. Anything that exists only in the tab's own session (temporary tables, `SET` values) is missing on the new session, and the statement then fails with the database's error.
- Only a single statement that SQLer classifies as a read is run again. Anything else (`INSERT`, `UPDATE`, DDL, a script with several statements, a statement SQLer cannot classify) is never run a second time: the *All rows* choice is not offered, a note gives the reason, and only the loaded rows can be exported.
- Memory does not depend on the number of rows; the writer stays a few batches behind the reader.
- Excel files hold at most 1,048,575 data rows, and a text cell at most 32,767 characters; a larger export fails with a message that suggests CSV or Parquet. Text that starts with `=`, `+`, `-` or `@` is written as text, never as a formula.
- A Parquet file is only complete when the export ends; its progress counts rows, and the byte count moves in steps.

**Parquet** is part of the default build of the desktop app, so it is in the format list. A build made without the `export-parquet` feature does not list it; use `sqler-cli export --format parquet` or another format there.

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
