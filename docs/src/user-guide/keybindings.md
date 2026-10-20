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

Each row shows the command title, its ID and its current shortcut. The palette shows the effective shortcut in canonical form, with `mod` resolved for your platform. For example, `mod+k` in `keybindings.toml` is shown as `cmd+k` on macOS and `ctrl+k` elsewhere, and modifiers are put in a fixed order. A shortcut that lost a conflict is not shown. Clicking a row also runs it.

The palette lists commands that need no argument. `connection:select`, `tab:activate`, `settings:set-theme` and `settings:set-ui-font-size` are not listed, because they need a value (an entry, a tab, a theme or a size) and are driven by clicks and the Settings tab. `connection:open` opens the connection selected in the sidebar and does nothing when none is selected. It has no default shortcut.

## Default shortcuts

| Command | macOS | Linux and Windows |
| ------- | ----- | ----------------- |
| `workspace:open-command-palette` | Cmd+Shift+P | Ctrl+Shift+P |
| `tab:close` | Cmd+W | Ctrl+W |
| `tab:next` | Ctrl+Tab | Ctrl+Tab |
| `tab:previous` | Ctrl+Shift+Tab | Ctrl+Shift+Tab |
| `app:quit` | Cmd+Q | Ctrl+Q |
| `settings:open` | Cmd+, | Ctrl+, |

`theme:toggle-mode` has no default shortcut; run it from the palette or bind it yourself.

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
