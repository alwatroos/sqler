# Results grid

The results grid shows rows in a table that draws only the rows you can see, so a result with a million rows scrolls as smoothly as one with a hundred. The same grid shows the results of a query (the Results view of the bottom panel, see [Running queries](running-queries.md)) and the data of a table (a tab opened with **Open Data** in the [object browser](object-browser.md)). This page is about the grid; [Export and copy](export.md) and [Inline editing](inline-editing.md) have their own pages.

## Open a grid

- Run a statement in a query tab; its rows appear in the Results view.
- In the object browser, open the context menu of a table or view and choose **Open Data**.
- To try scrolling on a million rows without a database, open the command palette (Cmd+Shift+P on macOS, Ctrl+Shift+P on Linux and Windows) and run `dev:open-synthetic-grid` ("Open Synthetic 1M-Row Grid"). It uses the connection of the active tab, or the connection selected in the sidebar when no tab is open; with neither, nothing happens. The grid tab is titled `Results: <connection name>`. It does not open a database session.

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

See [Export and copy](export.md#copy). The commands are `grid:copy` (Mod+C), `grid:copy-with-headers` (Mod+Shift+C), `grid:copy-as-markdown`, `grid:copy-as-json` and `grid:copy-as-insert`.

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

## Export

See [Export and copy](export.md#export). **Export Results...** (`grid:export`) writes the loaded rows, a selection or the whole result to a file.

## Editing

See [Inline editing](inline-editing.md). A grid is editable when SQLer can prove which table and row each cell belongs to; changes are staged, previewed and applied in one transaction.

## Measure it yourself

- `cargo run --release -p sqler-ui --example grid_scroll` scrolls 1,000,000 rows headless and prints, as `key=value` lines, the most rows needed in one frame and the data cost per frame. Flags: `--rows`, `--columns`, `--batch`, `--height`, `--row-height`, `--step`, `--overscan`.
- `SQLER_GRID_PROBE=1 sqler` opens the synthetic grid on the first saved connection, scrolls it in the real window and prints `SQLER grid_*` lines (frame intervals, rows drawn per frame, memory), then exits with 0 (pass), 1 (fail) or 2 (timeout after 120 s). It needs a saved connection.

The recorded numbers are on the [Benchmarks](../benchmarks.md) page.
