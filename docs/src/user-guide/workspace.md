# Workspace

`sqler` opens a window with a connection sidebar on the left and a tab strip on the right. A query tab has a plain multi-line input and a results area; see [Running queries](running-queries.md). The SQL editor with highlighting and completion comes later in M3.

```sh
cargo run -p sqler-app
```

Options: `-h`/`--help`, `-V`/`--version`, and `--exit-after-first-frame` (exits with code 0 once the first frame is shown; used to measure cold start).

## Saved connections

The sidebar lists the connections from `connections.toml` in the config directory, in file order. The file location is described in [Settings](settings.md) and the format in [Connections](connections.md). The connection store edits and saves the file, and you can also edit it by hand. If the file is missing or invalid, the sidebar shows the error text, and the app still starts. With no connections it shows: "No saved connections. Add them to connections.toml in the config directory (see User guide > Settings)."

Each row shows the name, the driver, a dot in the connection colour and an environment badge. While a connection opens the row shows "connecting..."; if it fails, the row shows the error text.

## Opening tabs

- Single click: select a connection.
- Double click on a row, or "Open in new tab" on the selected row: open a session and a new tab bound to it. The window stays responsive while the connection opens.

You can open several tabs on the same connection. Each tab has its own session. The second tab is titled `name (2)`, the third `name (3)`, and so on.

## Tab headers

A tab header shows the connection name, a stripe in the connection `color`, and a badge for the `environment`: `DEV`, `STAGING` or `PROD`. A `prod` tab looks different on purpose: its background and badge use the theme's danger colour, so you do not run a statement on production by mistake.

## Switching and closing

- Click a tab to switch to it; click its close button to close it. Closing the active tab activates the tab on its right, or on its left if it was the last.
- Keyboard defaults (see [Settings](settings.md)): the commands `tab:next`, `tab:previous`, `tab:close` and `app:quit` use the default keymap. Wiring the keymap to the window is a later task, so in this version only mouse actions work in the window.
- Closing the window quits the app.

## Known limits

- On quit open sessions are closed (ADR-0016): `app:quit` waits up to 2 s, the window-close hook only about 150 ms; a session that does not close in time is dropped.
- The interactive check on macOS (clicking, two tabs on one connection, switching, closing, the prod look) is still pending for the maintainer.
