# Themes

SQLer has two looks, "SQLer Light" and "SQLer Dark": flat surfaces, thin borders, one blue accent and line icons. You choose the look and the size of the interface text on the Settings tab. Both are kept in `settings.toml` ([Settings](settings.md#settingstoml)).

## Theme

Open the Settings tab (see [Settings](settings.md#settings-tab)). Under **Appearance**, the row **Theme** offers three choices:

| Choice | Effect |
| ------ | ------ |
| **Light** | Always the light theme. |
| **Dark** | Always the dark theme. |
| **Follow system** | Light or dark, as the OS appearance says. |

The key is `theme` in `settings.toml` with the values `light`, `dark` and `system` (the default).

"Follow system" also follows OS changes while SQLer is running. On Linux this depends on the desktop portal. Without it, SQLer cannot read the OS appearance and uses the light theme.

### Toggle between light and dark

The command `theme:toggle-mode` ("Toggle Light/Dark Theme") switches to the other look. It has no default shortcut and no button: run it from the [command palette](command-palette.md) or bind a chord to it in `keybindings.toml` ([Keybindings](keybindings.md#rebind-a-shortcut)). The command stores an explicit `light` or `dark`, the opposite of what you see now, so it leaves "Follow system". Choose **Follow system** on the Settings tab to go back.

## UI font size

On the Settings tab, the row **UI font size** has the buttons **−** and **+**, with the current size shown in points (for example "14 pt") and the range "8 to 48". The size is in whole points from 8 to 48 (default 14); the key is `ui_font_size`. Every text size of the interface follows it.

## Saving

A change applies at once, without a restart, and is saved to `settings.toml` at once. There is no Save button. The next start restores the theme and font size.

If `settings.toml` cannot be read (for example, it has a wrong key) or SQLer finds no config directory, SQLer uses the defaults and does not write the file, so your hand-edited file is not overwritten. The Settings tab then shows the problem and the line "Changes apply to this session only." If a save fails, the tab shows the error. A later successful save clears it.

## Environment colours

A connection can be marked with an environment: none, `dev`, `staging` or `prod` (see [Connections](connections.md#the-connection-form)). SQLer shows it as a chip with one of three accents, in both themes:

| Environment | Chip | Accent |
| ----------- | ---- | ------ |
| `dev` | `DEV` | green |
| `staging` | `STAGING` | amber |
| `prod` | `PROD` | red, a solid fill with white text |

A connection with no environment shows no chip. The chip is in the connection row, on the tabs, in the status bar and in the dialogs that ask before a write on a production connection. Production is drawn in the strongest way on purpose: the active tab of a `prod` connection has a red stripe on top and the status bar turns red while it is active (see [Workspace](workspace.md#tab-bars-groups-and-preview-tabs)). The accents are not a setting. The connection's own colour (the dot) is a separate choice in the connection form.
