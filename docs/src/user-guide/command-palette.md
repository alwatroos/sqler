# Command palette

Every action in SQLer is a command with an ID such as `tab:close`. The command palette lists the commands, filters them as you type and runs the one you pick, so you can reach any action without a menu or a shortcut.

## Open it

- Press Cmd+Shift+P (macOS) or Ctrl+Shift+P (Linux, Windows). The command is `workspace:open-command-palette`.
- Click **Show All Commands** on the welcome view (the editor area with no tab open).

The palette is a panel at the top centre of the window with the input "Type a command". Pressing the shortcut again closes it.

## Use it

| Key | What it does |
| --- | ------------ |
| Cmd+Shift+P (macOS), Ctrl+Shift+P (Linux, Windows) | Open the palette. Pressing it again closes the palette. |
| Type | Filter the commands. The best match by title or ID comes first. |
| Up, Down | Move the selection. It wraps from the last entry to the first and back. |
| Enter | Run the selected command and close the palette. |
| Esc | Close the palette without running anything. |

Clicking a row also runs it.

Each row shows the command title, its ID and its current shortcut as key chips. The palette shows the effective shortcut with `mod` resolved for your platform and the modifiers in a fixed order. For example, `mod+k` in `keybindings.toml` is shown as `⌘K` on macOS and `Ctrl+K` elsewhere. A shortcut that lost a [conflict](keybindings.md#conflicts) is not shown.

With an empty search box the commands you used last are listed first under "Recently used" (up to 10), followed by "All commands"; once you type, the list is one ranked list. The recent commands are kept between runs in `workbench.toml` ([Workspace](workspace.md#window-and-layout-memory)). When nothing matches, the palette says "No matching commands".

## What it lists

The palette lists the commands that need no argument. `connection:select`, `tab:activate`, `settings:set-theme`, `settings:set-ui-font-size`, `pane:focus`, `tab:move`, `workbench:set-side-width` and `workbench:set-panel-height` are not listed, because they need a value (an entry, a tab, a theme, a size, a width) and are driven by clicks and the Settings tab. `connection:open` opens the connection selected in the sidebar and does nothing when none is selected. It has no default shortcut.

Commands from other parts of the application appear in the same list with their own prefix in the title, for example:

- `Query: ...` ([Running queries](running-queries.md)), `Editor: ...` ([SQL editor](sql-editor.md)) and `Results: ...` ([Results grid](results-grid.md)) commands act on the active tab and need an open editor or result.
- `View: ...` commands show or hide parts of the [workbench](workspace.md).
- Commands of a plugin that is switched on and allowed appear as `<plugin name> (plugin): <title>`; see [Plugins](plugins.md#what-plugins-add).

Which chord runs a command, and how to change it, is on the [Keybindings](keybindings.md) page. The list of default shortcuts is there too.
