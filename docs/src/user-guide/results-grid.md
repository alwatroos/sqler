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
- **Top and End.** The buttons jump to the first and the last row. The same actions are the palette commands `grid:scroll-to-top` and `grid:scroll-to-end`, which act on the active grid tab. They have no default shortcut.

You can scroll while the rows are still loading. Loading runs on its own thread and never blocks the window.

## Close a grid

Closing the tab cancels the loading and frees the rows. Each synthetic grid holds about 235 MB, so close grids you no longer need.

## Not yet available

Sorting, filtering, resizing and moving columns, editing cells and exporting arrive in M4. Column widths are fixed by type.

## Measure it yourself

- `cargo run --release -p sqler-ui --example grid_scroll` scrolls 1,000,000 rows headless and prints, as `key=value` lines, the most rows needed in one frame and the data cost per frame. Flags: `--rows`, `--columns`, `--batch`, `--height`, `--row-height`, `--step`, `--overscan`.
- `SQLER_GRID_PROBE=1 sqler` opens the synthetic grid on the first saved connection, scrolls it in the real window and prints `SQLER grid_*` lines (frame intervals, rows drawn per frame, memory), then exits with 0 (pass), 1 (fail) or 2 (timeout after 120 s). It needs a saved connection.

The recorded numbers are on the [Benchmarks](../benchmarks.md) page.
