# Keybindings and the command palette

Every action in SQLer is a command with an ID such as `tab:close`. You can run a command from the command palette or with its keyboard shortcut, and you can rebind any shortcut.

## Command palette

| Key | What it does |
| --- | ------------ |
| Cmd+Shift+P (macOS), Ctrl+Shift+P (Linux, Windows) | Open the palette. Pressing it again closes the palette. |
| Type | Filter the commands. The best match by title or ID comes first. |
| Up, Down | Move the selection. It wraps from the last entry to the first and back. |
| Enter | Run the selected command and close the palette. |
| Esc | Close the palette without running anything. |

Each row shows the command title, its ID and its current shortcut as key chips. The palette shows the effective shortcut with `mod` resolved for your platform and the modifiers in a fixed order. For example, `mod+k` in `keybindings.toml` is shown as `⌘K` on macOS and `Ctrl+K` elsewhere. With an empty search box the commands you used last are listed first under "Recently used" (up to 10), followed by "All commands"; once you type, the list is one ranked list. When nothing matches, the palette says "No matching commands". A shortcut that lost a conflict is not shown. Clicking a row also runs it.

The palette lists commands that need no argument. `connection:select`, `tab:activate`, `settings:set-theme` and `settings:set-ui-font-size` are not listed, because they need a value (an entry, a tab, a theme or a size) and are driven by clicks and the Settings tab. `connection:open` opens the connection selected in the sidebar and does nothing when none is selected. It has no default shortcut.

## Default shortcuts

| Command | macOS | Linux and Windows |
| ------- | ----- | ----------------- |
| `workspace:open-command-palette` | Cmd+Shift+P | Ctrl+Shift+P |
| `tab:close` | Cmd+W | Ctrl+W |
| `tab:next` | Ctrl+Tab | Ctrl+Tab |
| `tab:previous` | Ctrl+Shift+Tab | Ctrl+Shift+Tab |
| `tab:move-left` | Cmd+Shift+PageUp | Ctrl+Shift+PageUp |
| `tab:move-right` | Cmd+Shift+PageDown | Ctrl+Shift+PageDown |
| `workbench:toggle-side-panel` | Cmd+B | Ctrl+B |
| `workbench:show-connections` | Cmd+Shift+E | Ctrl+Shift+E |
| `workbench:toggle-bottom-panel` | Cmd+J | Ctrl+J |
| `workbench:focus-next-region` | F6 | F6 |
| `workbench:focus-previous-region` | Shift+F6 | Shift+F6 |
| `pane:split-right` | Cmd+\ | Ctrl+\ |
| `app:quit` | Cmd+Q | Ctrl+Q |
| `settings:open` | Cmd+, | Ctrl+, |
| `editor:new` | Cmd+N | Ctrl+N |
| `editor:format-sql` | Shift+Alt+F | Shift+Alt+F |
| `editor:add-next-occurrence` | Cmd+D | Ctrl+D |
| `editor:trigger-completion` | Ctrl+Space | Ctrl+Space |
| `query:run-selection` | Cmd+E | Ctrl+E |
| `query:run-statement` | Cmd+Enter | Ctrl+Enter |
| `query:run-all` | Cmd+Shift+Enter | Ctrl+Shift+Enter |
| `query:cancel` | Cmd+. | Ctrl+. |

The `query:*` commands are described in [Running queries](running-queries.md).

The `workbench:*` and `pane:*` commands are described in [Workspace](workspace.md). The other workbench commands have no default shortcut: `workbench:show-saved-queries`, `workbench:show-results`, `workbench:show-messages`, `workbench:reset-layout`, `pane:focus-next`, `pane:focus-previous`, `pane:move-tab-to-next`, `pane:move-tab-to-previous`, `tab:pin`, `tab:close-others`, `tab:close-to-the-right` and `tab:close-all`. `workbench:set-side-width`, `workbench:set-panel-height` and `pane:focus` take a value and are driven by the mouse.

Keyboard in the workbench: `F6` and `Shift+F6` cycle the regions (activity bar, side panel, editor, bottom panel, status bar). Tab and Shift+Tab move between the controls of the activity bar and the status bar, Enter or Space activates the focused control, and Escape in the bottom panel returns to the editor.

`theme:toggle-mode` has no default shortcut; run it from the palette or bind it yourself.

## SQL editor

The editor commands are in the palette under `Editor:`. The four above have default shortcuts; `editor:add-cursor-above`, `editor:add-cursor-below` and `editor:go-to-matching-bracket` have none, so run them from the palette or bind them in `keybindings.toml`.

| Command | Title |
| ------- | ----- |
| `editor:new` | Editor: New SQL Editor |
| `editor:format-sql` | Editor: Format SQL |
| `editor:add-next-occurrence` | Editor: Add Next Occurrence |
| `editor:add-cursor-above` | Editor: Add Cursor Above |
| `editor:add-cursor-below` | Editor: Add Cursor Below |
| `editor:go-to-matching-bracket` | Editor: Go to Matching Bracket |
| `editor:trigger-completion` | Editor: Trigger Completion |
| `dev:open-large-sql-script` | Open Synthetic 10,000-Line SQL Script |

On macOS Ctrl+Space is also the default shortcut for switching the input source, so the system can intercept it. Turn that system shortcut off or rebind `editor:trigger-completion` in `keybindings.toml`; the command is also in the palette. See [Intellisense](sql-editor.md#intellisense).

While the editor has focus, the editor component also handles its own keys. These are not SQLer commands and cannot be rebound in `keybindings.toml`:

| Action | macOS | Windows | Linux |
| ------ | ----- | ------- | ----- |
| Add a cursor | Alt-click | Alt-click | Alt-click |
| Add cursor above | Cmd+Alt+Up | Ctrl+Alt+Up | Shift+Alt+Up |
| Add cursor below | Cmd+Alt+Down | Ctrl+Alt+Down | Shift+Alt+Down |
| Undo | Cmd+Z | Ctrl+Z | Ctrl+Z |
| Redo | Cmd+Shift+Z | Ctrl+Y | Ctrl+Y |

Chords bound by SQLer are taken first, so a chord you bind in `keybindings.toml` wins over the editor's own key. See [SQL editor](sql-editor.md).

## Results grid

While a results grid has the focus, these keys select cells. `Mod` is Cmd on macOS and Ctrl elsewhere.

| Keys | Command |
| ---- | ------- |
| Up, Down, Left, Right | `grid:move-up`, `grid:move-down`, `grid:move-left`, `grid:move-right` |
| Home, End | `grid:move-row-start`, `grid:move-row-end` |
| Mod+Home, Mod+End | `grid:move-first-cell`, `grid:move-last-cell` |
| Page Up, Page Down | `grid:move-page-up`, `grid:move-page-down` |
| Tab, Shift+Tab | `grid:move-right`, `grid:move-left`, only while that moves the active cell (at the last or first cell the key goes on, so Tab leaves the table) |
| Shift with any of the movement keys above except Tab | the matching `grid:extend-*` command |
| Mod+A | `grid:select-all` |
| Shift+Space | `grid:select-row` |
| Ctrl+Space | `grid:select-column` (Ctrl on every system, also on macOS) |
| Mod+C | `grid:copy` (the selection as tab-separated text) |
| Mod+Shift+C | `grid:copy-with-headers` |

These in-grid keys are fixed: they are not in `keybindings.toml` and cannot be rebound. The commands they run are palette commands, and you can also give them global chords. The grid gets no global default shortcut, because every useful chord (Mod+A, Mod+C, Alt or Mod with arrows, Mod+Home and Mod+End) is a text-editing chord in the SQL editor. Mod+C and Mod+Shift+C are therefore not default chords either: the grid takes them only while a results table has the focus, so copying in the SQL editor is unchanged.

The grid takes these keys before the global chords, but only while a results table has the focus. That is why Ctrl+Space, which is bound to `editor:trigger-completion` by default, selects the column in a results grid and still triggers completion in the SQL editor. A key the grid does not use (for example Mod+W or Ctrl+Tab) goes on to your global chords as usual. While the palette or a dialog is open, the grid takes no keys.

The commands without an in-grid key are `grid:copy-as-markdown`, `grid:copy-as-json`, `grid:copy-as-insert`, `grid:pin-column`, `grid:unpin-column`, `grid:move-column-left`, `grid:move-column-right`, `grid:autofit-column`, `grid:reset-layout`, `grid:scroll-to-top`, `grid:scroll-to-end`, `grid:fetch-more` and `grid:toggle-cell-detail` (opens the [cell detail](results-grid.md#cell-detail) pane; it has no default chord because Mod+Shift+I is the GPUI inspector on Linux and Windows). All `grid:*` commands act on the active results grid, or on the result a query tab shows. See [Results grid](results-grid.md).

## Rebind a shortcut

Edit `keybindings.toml` in the config directory. [Settings](settings.md) lists where the file lives and the full chord grammar (modifiers, keys, `mod`).

```toml
[bindings]
# Replace the default chords of a command (several chords are allowed)
"workspace:open-command-palette" = ["mod+shift+p", "f1"]
"tab:close" = ["mod+k"]
# An empty list removes the shortcut; the command stays in the palette
"app:quit" = []
```

- An entry replaces all default chords of that command.
- `mod` means Cmd on macOS and Ctrl elsewhere.
- Changes are read at startup. Restart SQLer after editing the file.
- If the file cannot be read (for example a typo in a key), SQLer uses the default shortcuts and tells you why.
- An entry for a command ID that SQLer does not know is ignored and reported.

## Conflicts

If two commands share a chord, SQLer reports the conflict in two places: a line on standard error (`sqler: Shortcut ctrl+k is bound to a:b, c:d; it runs a:b`) and a notification in the window. The conflict does not stop SQLer from starting.

Exactly one command runs for a conflicting chord:

1. A command whose entry in `keybindings.toml` lists the chord wins, so your choice beats a default.
2. Otherwise the command that comes first in ID order (alphabetical) wins.

To remove the conflict, give the other command a different chord or an empty list.
