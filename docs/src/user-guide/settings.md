# Settings

SQLer reads three TOML files from one config directory. You can edit them by hand. A missing file is
fine: SQLer then uses the defaults. A wrong key or value is reported with the file name and the
problem; it is never silently ignored.

## Where the files are

| OS | Config directory |
| -- | ---------------- |
| Linux and other | `$XDG_CONFIG_HOME/sqler` (if set and absolute), otherwise `~/.config/sqler` |
| macOS | `~/Library/Application Support/SQLer` |
| Windows | `%APPDATA%\SQLer` |

Files in that directory: `settings.toml`, `keybindings.toml`, `connections.toml`.

## settings.toml

```toml
theme = "system"
ui_font_size = 14
```

| Key | Values | Default |
| --- | ------ | ------- |
| `theme` | `light`, `dark`, `system` (follow the OS) | `system` |
| `ui_font_size` | whole points, 8 to 48 | `14` |

Unknown keys and values outside the range are errors.

## keybindings.toml

```toml
[bindings]
"workspace:open-command-palette" = ["mod+shift+p", "f1"]
"tab:close" = []
```

- Each entry replaces the default chords of that command. An empty list unbinds the command.
- Command IDs look like `category:verb-object`, for example `settings:open`.
- A chord is modifiers and one key joined by `+`, for example `mod+shift+p`. Case does not matter.
  The key comes last, and a modifier may appear only once.
- Modifiers: `ctrl` (`control`), `alt` (`option`), `shift`, `cmd` (`super`) and `mod`. `mod` is
  `cmd` on macOS and `ctrl` on other systems.
- Keys: `a` to `z`, `0` to `9`, the punctuation keys `` ` - = [ ] \ ; ' , . / ``, `enter`, `escape`,
  `tab`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`,
  `left`, `right` and `f1` to `f24`. Aliases: `return` for `enter`, `esc` for `escape`.

Default bindings (`mod` shown as written; it resolves per platform):

| Command | Chords |
| ------- | ------ |
| `workspace:open-command-palette` | `mod+shift+p` |
| `settings:open` | `mod+,` |
| `tab:close` | `mod+w` |
| `tab:next` | `ctrl+tab` |
| `tab:previous` | `ctrl+shift+tab` |
| `app:quit` | `mod+q` |

### Conflicts

If two commands end up with the same chord, for example because you gave a command a chord that
another command already uses, SQLer reports a conflict that lists the chord and the commands. A
conflict is a warning and does not stop SQLer from starting.

## connections.toml

SQLer only reads this file in this version; editing connections in the app arrives in a later release.

```toml
[[connection]]
name = "local"
driver = "sqlite"
target = "app.db"
color = "#22aa66"
environment = "dev"
```

| Key | Required | Rule |
| --- | -------- | ---- |
| `name` | yes | not empty, unique |
| `driver` | yes | a driver ID such as `sqlite` |
| `target` | yes | file path or connection string, without a password |
| `color` | no | `#rrggbb` |
| `environment` | no | `dev`, `staging` or `prod` |

A target that contains a secret is rejected, and the error does not repeat the target. This covers
any user part in a `scheme://` URL (`user@` as well as `user:password@`) and any `password`, `pwd`,
`secret` or `token` `key=value` parameter, in any letter case. Drop the user part from the target;
user names and passwords are not stored in config files, and the system keychain and connection
settings arrive in M2.

See [ADR-0005](../adr/0005-config-files-and-keybindings.md) for the design.

## Settings tab and themes

SQLer has a Settings tab, so you do not need to edit `settings.toml` by hand.

### Opening it

- Click the **Settings** button at the right end of the tab strip.
- Run the command `settings:open` ("Open Settings").
- Press `mod+,` once keybindings are available.

The Settings tab is one tab. While it is open, it replaces the tab body, and the Settings header in
the tab strip has a **×** button that closes it (the command `tab:close` closes it as well). Your
connection tabs stay open behind it.

### Theme

The tab offers three choices:

| Choice | Effect |
| ------ | ------ |
| Light | Always the light theme. |
| Dark | Always the dark theme. |
| Follow system | Light or dark, as the OS appearance says. |

"Follow system" also follows OS changes while SQLer is running. On Linux this depends on the desktop
portal. Without it, SQLer cannot read the OS appearance and uses the light theme.

The **Dark** / **Light** button at the right end of the tab strip switches to the other mode. It runs
the command `theme:toggle-mode` ("Toggle Light/Dark Theme"). The command stores an explicit `light`
or `dark`, the opposite of what you see now, so it leaves "Follow system". Choose "Follow system" in
the Settings tab to go back.

### UI font size

Use the **−** and **+** buttons to change the size of the interface text. The size is in whole
points, from 8 to 48 (default 14).

### Saving

A change applies at once, without a restart, and is saved to `settings.toml` at once. There is no
Save button. The next start restores the theme and font size.

If `settings.toml` cannot be read (for example, it has a wrong key) or SQLer finds no config
directory, SQLer uses the defaults and does not write the file, so your hand-edited file is not
overwritten. The Settings tab then shows the problem and the line "Changes apply to this session
only." If a save fails, the tab shows the error. A later successful save clears it.
