# Export and copy

You can take rows out of a [results grid](results-grid.md) in two ways. **Copy** puts the selected cells on the clipboard. **Export** writes a result to a file, either what the grid holds or the whole result read again from the database. From a terminal, [`sqler-cli export`](cli.md#export) does the same without the window.

## Copy

Copy the selected cells (or a row, a column, or everything with Mod+A) to the clipboard. The text follows what the grid shows: the column order and pinned columns as you arranged them, and the row order of the grid. Mod+C works while the grid has the focus; every copy is also in the palette, and in the menu of a right click on a row. A right click does not change the selection.

| Command | Menu | Result |
| ------- | ---- | ------ |
| `grid:copy` (Mod+C) | Copy | Tab-separated values, no header |
| `grid:copy-with-headers` (Mod+Shift+C) | Copy with Headers | The same, with the column names on the first line |
| `grid:copy-as-markdown` | Copy as Markdown | A Markdown table |
| `grid:copy-as-json` | Copy as JSON | An array of objects, one per row |
| `grid:copy-as-insert` | Copy as SQL INSERT | `INSERT` statements, 100 rows each |

In the palette the titles start with "Results: ", for example "Results: Copy as Markdown".

The copy uses the same writers as file export. In TSV, a value with a tab, a line break or a double quote is wrapped in double quotes (quotes doubled), so spreadsheets paste it into one cell; a single cell is copied without a trailing line break. NULL is copied as `NULL` in TSV, Markdown and `INSERT`, and as `null` in JSON; binary data is copied as hexadecimal text such as `0x00ff` in TSV and Markdown (`INSERT` and JSON use their own notation).

- **Empty strings and the text `NULL`.** NULL is copied as `NULL`, so a text value `NULL` is wrapped in quotes (`"NULL"`) to tell them apart; a text editor shows those quotes, a spreadsheet does not. When one column is copied and some values are empty strings, those rows are blank lines, and a single empty string is an empty clipboard; some paste targets drop blank lines, so copy a second column with them or use JSON.
- **`INSERT`.** It uses the SQL dialect of the tab's connection, so SQL Server gets `[name]` quoting. A grid opened with "Open data" on a table or view uses that table's real (qualified) name; any other result, such as a query result, uses the placeholder `table_name`, which you edit after pasting. SQLer does not ask for a name. A connection that has no SQL dialect (a plugin driver that declares none) gets PostgreSQL quoting, and SQLer says so.
- **Rows fetched so far.** A copy takes the rows that are loaded. If the result is still loading or paused and your selection reaches the last loaded row, the notification says "Only the rows fetched so far were copied." Fetch more rows first if you need them all.
- **Limits.** A copy holds at most 1,000,000 cells and 50 MiB of text. Over a limit nothing is copied and a message says to select fewer rows or columns.
- **Large copies.** A large selection is read in slices and rendered in the background, so the window stays responsive; a copy of 100,000 cells or more, a partial copy and an `INSERT` with an assumed dialect show a notification when done. Starting another copy replaces the one in progress.
- **Formulas.** The text is copied exactly as stored. A spreadsheet may run a pasted value that starts with `=`, `+`, `-` or `@` as a formula, so paste data you do not trust into a text editor first.

The copied text is only put on the clipboard; SQLer does not log it or show it in notifications. The SQL editor keeps its own Mod+C.

## Export

**Export Results...** (`grid:export`, in the command palette) writes the result of the active grid or query result to a file. It works on a table-data tab and on every result of a query tab. The dialog is titled "Export results" and offers:

- **Format**: CSV, JSON, SQL INSERT and Markdown table in every build. Excel (`.xlsx`) and Parquet are listed only when the build has them (the `export-xlsx` and `export-parquet` features); both are in the default desktop build and in the downloads. Every 1.0 download also has the plugin host, so the formats of plugins that are switched on follow the built-in ones as `<plugin name> (plugin): <label>`; they have no options, and a failing plugin ends the export with a short message and no file (see [Plugins](plugins.md#what-plugins-add) and [Exporters](../plugin-development/exporters.md)).
- **Scope**: *Loaded rows (N)* (when the result is complete, *All N rows*), the rows the grid holds now, as displayed (with the current sort and filter, in the order you see them; N is the number of rows shown), *Selection (R × C)* when cells are selected, copied in the order you see them, and *All rows (runs the query again)*.
- **Options**: for CSV the delimiter (`,` `;` Tab `|`), **Header row** and the NULL text; for Markdown the NULL text; for SQL the table name, an optional schema and the rows per `INSERT` (1 to 10,000). JSON, Parquet and Excel use their defaults.

After **Export...** the system save dialog asks for the file. A progress line with a **Cancel** button (also `export:cancel`, "Cancel Export") is shown while the file is written. The file is written under a hidden name next to the target and renamed when the export is complete, so a cancelled or failed export leaves no half-written file and an existing file is replaced only by a complete one. Replacing a file keeps its permissions on Unix; a symbolic link at the target is replaced by a regular file and the file it pointed to is left alone. If the result changes while the dialog is open (the query runs again, the table is opened again), the export is cancelled with a notice. Closing the tab or quitting the app cancels its export.

Things to know:

- *Loaded rows* exports what the grid shows: when a sort or filter is set, only the matching rows, in the sorted order, exactly as copy and the selection export do. *All rows (runs the query again)* ignores the grid's sort and filter.
- *Loaded rows* covers what the grid holds. A result that is paused at its page limit (see [Pages and paused results](results-grid.md#pages-and-paused-results)) exports only the loaded pages; the dialog shows the count.
- *All rows (runs the query again)* runs the statement once more on a new, separate session and streams every row into the file, without loading them into the grid. The data can differ from what the grid shows, because it is read again now. Anything that exists only in the tab's own session (temporary tables, `SET` values) is missing on the new session, and the statement then fails with the database's error.
- Only a single statement that SQLer classifies as a read is run again. Anything else (`INSERT`, `UPDATE`, DDL, a script with several statements, a statement SQLer cannot classify) is never run a second time: the *All rows* choice is not offered, a note gives the reason, and only the loaded rows can be exported.
- Memory does not depend on the number of rows; the writer stays a few batches behind the reader.
- Excel files hold at most 1,048,575 data rows, and a text cell at most 32,767 characters; a larger export fails with a message that suggests CSV or Parquet. Text that starts with `=`, `+`, `-` or `@` is written as text, never as a formula.
- A Parquet file is only complete when the export ends; its progress counts rows, and the byte count moves in steps.

**Parquet** is part of the default build of the desktop app, so it is in the format list. A build made without the `export-parquet` feature does not list it; use another format there.

### SQL Server

Export works on SQL Server, including the fresh run. SQL `INSERT` files hold at most 1000 rows per statement and write text as `N'..'`. The limits of that driver (only the first result set, `money` as a float) apply to the exported values too; see [SQL Server](databases/sqlserver.md).

### Plugin formats

A plugin that declares an exporter adds its format to the dialog and to `sqler-cli export --format plugin-<id>:<fmt>`. The plugin only turns rows into bytes; SQLer writes the file. See [What plugins add](plugins.md#what-plugins-add).

### From the command line

`sqler-cli export` streams one statement into a file with the same writers; see [Command-line interface](cli.md#export).
