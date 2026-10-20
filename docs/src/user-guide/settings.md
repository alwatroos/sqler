# Settings

SQLer keeps its files in one config directory: three settings files (`settings.toml`, `connections.toml`, `keymap.toml`), the saved queries (`queries.toml`, also TOML) and the query history (`history.jsonl`). You can edit the TOML files by hand. A missing file is
fine: SQLer then uses the defaults. A wrong key or value is reported with the file name and the
problem; it is never silently ignored.

## Where the files are

| OS | Config directory |
| -- | ---------------- |
| Linux and other | `$XDG_CONFIG_HOME/sqler` (if set and absolute), otherwise `~/.config/sqler` |
| macOS | `~/Library/Application Support/SQLer` |
| Windows | `%APPDATA%\SQLer` |

Files in that directory: `settings.toml`, `keybindings.toml`, `connections.toml`, `history.jsonl`, `queries.toml`.

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

The saved connections, their groups, TLS and SSH settings are described in [Connections](connections.md).

## history.jsonl

SQLer records every executed query in `history.jsonl`: one JSON object per line, with the SQL, the
connection ID, the start time, the duration, the outcome (`ok`, `error` or `cancelled`) and the row
count when known. You do not need to edit it. The format is in
[ADR-0021](../adr/0021-query-history-and-saved-queries.md).

```json
{"v":1,"started_ms":1760950000123,"connection":"billing","duration_ms":42,"outcome":"ok","rows":10,"sql":"select 1"}
```

- Retention: the newest 10,000 entries are kept by default (the limit can be set from 1 to 100,000).
  The file is trimmed to that limit now and then, not on every query. SQL longer than 256 KiB is cut
  and the entry gets `"truncated":true`.
- Search is by text (every word must appear in the SQL, any case) and can be limited to one
  connection. The newest entries come first.
- A line that cannot be read, or that was written by a newer SQLer, is reported with its line number
  and left in the file. The other lines still load. Clearing the history removes them.
- History can be switched off, and it can be cleared completely or for one connection.
- Use one SQLer window process at a time. Two running instances do not coordinate, and one may lose
  lines that the other wrote while the file was being trimmed.

### Privacy

The history stores the SQL as you typed it. That can include literals such as names, e-mail
addresses or a password written inside a statement. It never stores bound parameter values, the
passwords of your connections, or error messages.

Before saving, SQLer hides credentials it recognises. A statement counts as sensitive if it
contains one of the words `PASSWORD`, `IDENTIFIED`, `SECRET`, `SECRET_KEY`, `AWS_SECRET_ACCESS_KEY`,
`API_KEY`, `TOKEN`, `AUTHORIZATION` or `BEARER` outside quotes and comments, or a quoted text with
`password=`, `pwd=`, a similar key or a URL with a password. In such a statement every `'...'` text,
every `"..."` text (a string in `MySQL`), every dollar-quoted text and every comment is saved as
`'<redacted>'`. Because dialects read backslashes and comments differently, SQLer checks the statement
under each reading and hides everything any reading would treat as quoted. Other statements, with
their comments, are saved as typed. This is a best-effort rule: it can hide more than needed, and a
credential in another form is stored as typed. If that is a problem, switch the history off and
clear it.

The file is plain text. Do not put the config directory in a public dotfiles repository or another
synced place without excluding `history.jsonl` and `queries.toml`.

## queries.toml

Saved queries are kept in `queries.toml`. You can edit it by hand.

```toml
version = 1

[[query]]
id = "q-1"
name = "Open invoices"
folder = "billing/reports"
connection = "billing"
tags = ["invoices", "monthly"]
sql = """
select * from invoices where paid = false
"""
```

| Key | Meaning |
| --- | ------- |
| `version` | Required, `1`. A higher number is refused: "written by a newer SQLer". |
| `id` | Required. Lowercase letters, digits and `-`, at most 64 characters, unique. New queries get `q-1`, `q-2`, and so on. Do not change it. |
| `name` | Required. Not empty, and unique within its folder. |
| `sql` | Required. The statement text. |
| `folder` | Optional. A path such as `billing/reports`; no empty parts. |
| `connection` | Optional. The ID of the connection to use. If that connection is gone, the value stays. |
| `tags` | Optional. A list of non-empty words; duplicates are dropped. |

Unknown keys are errors. The list shows queries in file order, and a new query goes to the end.
SQLer writes the whole file when you save, so comments and spacing in a hand-edited file are not
kept. If the file cannot be read, SQLer reports the problem and leaves the file as it is.

## Settings tab and themes

SQLer has a Settings tab, so you do not need to edit `settings.toml` by hand.

### Opening it

- Click the **Settings** button at the right end of the tab strip.
- Run the command `settings:open` ("Open Settings").
- Press `mod+,` (Cmd+, on macOS, Ctrl+, elsewhere), or pick it from the [command palette](keybindings.md).

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
