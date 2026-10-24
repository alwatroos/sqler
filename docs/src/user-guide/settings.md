# Settings

SQLer keeps its files in one config directory: three settings files (`settings.toml`, `keybindings.toml`, `connections.toml`), the saved queries (`queries.toml`, also TOML) and the query history (`history.jsonl`). You can edit the TOML files by hand. A missing file is
fine: SQLer then uses the defaults. A wrong key or value is reported with the file name and the
problem; it is never silently ignored.

## Where the files are

| OS | Config directory |
| -- | ---------------- |
| Linux and other | `$XDG_CONFIG_HOME/sqler` (if set and absolute), otherwise `~/.config/sqler` |
| macOS | `~/Library/Application Support/SQLer` |
| Windows | `%APPDATA%\SQLer` |

Files in that directory: `settings.toml`, `keybindings.toml`, `connections.toml`, `history.jsonl`, `queries.toml`, `ai.toml` (AI providers and limits, see [AI](ai.md)), `workbench.toml` (panel layout, window size and recent commands, see [Workspace](workspace.md#window-and-layout-memory)), `update-state.toml` (see [Updates](#updates)), `plugin-grants.toml` (plugin permissions, see [Plugins](plugins.md)), `plugin-prefs.toml` (which plugins are switched off and their setting values, see [Plugins](plugins.md)).

## settings.toml

```toml
theme = "system"
ui_font_size = 14
result_page_rows = 100000
```

| Key | Values | Default |
| --- | ------ | ------- |
| `theme` | `light`, `dark`, `system` (follow the OS) | `system` |
| `ui_font_size` | whole points, 8 to 48 | `14` |
| `result_page_rows` | whole rows per result page, 1000 to 10000000 | `100000` |
| `check_for_updates` | `true`, `false` (see [Updates](#updates)) | `false` |

`theme` and `ui_font_size` are described in [Themes](themes.md). `result_page_rows` is how many rows a query result loads before it pauses and offers **Fetch more** (see [Results grid](results-grid.md#pages-and-paused-results)). It is set in this file only; the Settings tab has no control for it.

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

A few default bindings (`mod` shown as written; it resolves per platform). The full list is on the [Keybindings](keybindings.md#default-shortcuts) page:

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

Before saving, SQLer hides credentials, and it errs on the side of hiding too much. This is a
heuristic, not a guarantee. A statement counts as sensitive if its text contains, anywhere and in
any case, a marker such as `passw` (so `PASSWORD`, `passwd`, `password_hash`), `pwd`, `secret`,
`token`, `credential`, `apikey`, `api_key`, `authorization`, `oauth`, `bearer`, `identified`,
`_key`, `salt`, `encrypt`, `decrypt`, the whole words `pw` and `key` (as in
`pgp_sym_encrypt(x, key)` or SQLCipher `ATTACH ... KEY`, not `PRIMARY KEY`), `private key`, or a URL
with user info (`scheme://...@`). The marker may even sit inside a comment, a quoted name or a
text. Bare `auth`, `ssl`, `cert`, `otp` and `hash` are not markers, since they appear in harmless
words such as `author`. In a sensitive statement every `'...'` text, every `"..."` text, every
dollar-quoted text, every backtick name, and every comment (including `MySQL` `/*! ... */` comments,
which run as SQL) is saved as `'<redacted>'`. Texts that hold JSON, YAML or `key=value` credentials
go away with the rest. Because dialects read quotes, backslashes and comments differently, SQLer
checks the statement under each reading, hides everything any reading would treat as quoted, and
hides everything after a quote or comment that is never closed. Other statements, with their
comments, are saved as typed. A statement longer than the size limit is cut first.

Hiding more than needed is by design, for example a harmless `SELECT` that mentions a column named
`token_count` loses its texts. Known gaps: a secret written without quotes, such as the Oracle form
`IDENTIFIED BY pw`; password hashes written as bare hex numbers (`MSSQL`); a secret behind a column
or variable name that carries no marker; and an encryption key given as a text to a function whose
name carries no marker (`encrypt`, `decrypt`). If that is a problem, switch the history off and
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

## Settings tab

SQLer has a Settings tab, so you do not need to edit `settings.toml` by hand. It has the sections **Appearance** (theme and UI font size, see [Themes](themes.md)), **AI** and **Updates**.

### Opening it

- Click the **Settings** icon (the gear) at the bottom of the activity bar.
- Run the command `settings:open` ("Open Settings").
- Press `mod+,` (Cmd+, on macOS, Ctrl+, elsewhere), or pick it from the [command palette](command-palette.md).

The Settings tab is one tab. While it is open, a Settings tab is shown at the right end of the tab strip of the first group, with a **×** button that closes it. Your connection tabs stay open behind it.

### AI

The page ends with the **AI** section (providers, models, redaction, limits), described in [AI](ai.md#settings--ai). The page scrolls.

### Updates

The **Updates** section follows the AI section. It is off until you turn it on.

- **Check for updates**: the switch. It stores `check_for_updates` in `settings.toml`. The text under it reads "Asks GitHub Releases for a signed version file about once a day. Nothing is downloaded or installed; no personal data is sent."
- **Status** with a **Check now** button: checks once, at once. The line next to it says "Not checked yet", "Checking…", "Up to date (latest X)", "SQLer X is available" or "Last check failed: …". The button is disabled while the switch is off or a check is running.

What a check sends: one `GET` of the signed version file on GitHub Releases, with the header `User-Agent: SQLer/<version> (update-check)` and nothing else. No query, no identifier, no hostname, no OS detail. The answer is verified with a public key built into SQLer. SQLer never downloads or installs a new version in 1.0; it only tells you.

When: with the switch on, the first check runs about 30 seconds after SQLer has drawn its first window, then about once a day. After a failure it retries after 1 hour, doubling up to 24 hours. Being offline is silent. While the switch is off, SQLer makes no request at all, and **Check now** and the command `app:check-for-updates` only show a hint.

When a newer version exists, the status bar shows **Update X**. Click it to open the release notes in your browser, or use the dismiss button next to it (`app:dismiss-update-notice`). A dismissed version is not shown again by the automatic check, also after a restart. The file `update-state.toml` in the config directory keeps: the time of the last attempt, the number of failures in a row and the dismissed version, nothing else.

To turn it off, switch **Check for updates** off, set `check_for_updates = false`, or run `settings:toggle-update-checks` ("Toggle Automatic Update Checks"). Turning it off also stops a check that is running. A build made without the `update` feature has no update client, and the section says so.

### Theme

See [Themes](themes.md#theme): **Light**, **Dark** or **Follow system**, and the command `theme:toggle-mode`.

### UI font size

See [Themes](themes.md#ui-font-size).

### Saving

See [Themes](themes.md#saving): a change applies and is saved at once.
