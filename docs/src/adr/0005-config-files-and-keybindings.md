# 0005 Config files and keybindings: TOML files, chord grammar, std-only directory lookup

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

SQLer needs persistent user configuration: settings (theme, font size), rebindable keyboard shortcuts and a list of saved connections. `sqler-config` (layer L1, depends only on `sqler-core`) provides one tested, framework-independent API that the desktop shell, the settings UI and the keybinding dispatch consume. It adds new dependencies (`serde`, `toml`) and a public API, so AGENTS.md §6 (step 2) and §8 require an ADR first.

## Decision drivers

- Human-editable files in the platform config directory.
- Typo-safe: a wrong key or value is reported with the file name and the problem, not ignored.
- Keybinding conflicts are reported, never fatal.
- Secrets never live in config files (keychain arrives in M2).
- `sqler-core` stays free of `serde`.
- Only licenses on the `deny.toml` allow-list.

## Considered options

1. TOML files parsed with `serde` and `toml`; directory lookup with `directories`.
2. The same, with a `std`-only directory lookup.
3. JSON or a custom format.

## Decision outcome

Chosen: option 2.

### File locations

One config directory holds three files: `settings.toml`, `keybindings.toml` and `connections.toml`.

| OS | Config directory |
| -- | ---------------- |
| Linux and other | `$XDG_CONFIG_HOME/sqler` if the variable is set and absolute, otherwise `~/.config/sqler` |
| macOS | `~/Library/Application Support/SQLer` |
| Windows | `%APPDATA%\SQLer` |

There is no environment override; callers and tests use `ConfigDir::new(path)`. A missing file is not an error: settings and keybindings fall back to defaults, and the connection list is empty. Saves create the directory and write atomically (temporary file, then rename).

### Formats

```toml
# settings.toml
theme = "system"        # light | dark | system
ui_font_size = 14       # whole points, 8..=48
```

```toml
# keybindings.toml: each entry replaces that command's default chords; [] unbinds
[bindings]
"workspace:open-command-palette" = ["mod+shift+p", "f1"]
```

```toml
# connections.toml (read-only in M1)
[[connection]]
name = "local"
driver = "sqlite"
target = "app.db"
color = "#22aa66"       # optional, #rrggbb
environment = "dev"     # optional: dev | staging | prod
```

Unknown keys in any file are errors (`deny_unknown_fields`), so typos surface. Errors are typed (`ConfigError`) and name the file and the problem: `Parse` for TOML syntax and type errors (with line and column), `Invalid` for semantic errors.

Connection rules: the name is non-empty and unique, the driver is a valid `DriverId`, the colour matches `#rrggbb` (stored lowercase), the environment is one of `dev`, `staging`, `prod`. A target that embeds a secret (any URL userinfo, including a bare user name, or a `password`, `pwd`, `secret` or `token` `key=value` parameter in any case) is rejected, and the error never echoes the target. `ConnectionConfig` gains optional `environment` and `color` fields in `sqler-core`; its `Debug` output still redacts the target.

### Keybinding schema

- Command IDs use the form `category:verb-object`: two segments joined by `:`, each segment matching `[a-z][a-z0-9]*(-[a-z0-9]+)*`.
- A chord is single-stroke and global: tokens joined by `+`, matched case-insensitively, exactly one key (last), no repeated modifier.
- Modifiers: `ctrl`/`control`, `alt`/`option`, `shift`, `cmd`/`super`, `mod`. `mod` means `cmd` on macOS and `ctrl` elsewhere and is never written back.
- Keys: `a`-`z`, `0`-`9`, `` ` - = [ ] \ ; ' , . / ``, `enter`/`return`, `escape`/`esc`, `tab`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `f1` to `f24`.
- Canonical form: `ctrl+alt+shift+cmd+<key>`, modifiers in that order, key in lowercase. Saved files use the canonical form.
- Override semantics: a user entry replaces all default chords of that command; an empty list unbinds it. Duplicate chords inside one entry are deduplicated. Well-formed IDs that are unknown to `sqler-config` are kept, because the command registry lives in `sqler-ui`.
- Conflicts: computed on the merged keymap. A chord bound to two or more commands is returned as a `Conflict` (chord plus sorted command IDs). Conflicts are reported, not fatal.

Built-in defaults (`mod` resolved per platform):

| Command | Chords |
| ------- | ------ |
| `workspace:open-command-palette` | `mod+shift+p` |
| `settings:open` | `mod+,` |
| `tab:close` | `mod+w` |
| `tab:next` | `ctrl+tab` |
| `tab:previous` | `ctrl+shift+tab` |
| `app:quit` | `mod+q` |

These command IDs are provisional until the commands are registered by the UI work (T4 to T6). They are pinned here; a change needs a new ADR or an amendment of this one.

### Dependencies

- `serde` 1.0.229 (`derive`) and `toml` 1.1.8, both `MIT OR Apache-2.0`, checked live on crates.io on 2026-10-20. See [Dependencies](../dependencies.md).
- `directories` 6.0.0 was evaluated and rejected. It pulls `dirs-sys` 0.5.0 (`MIT OR Apache-2.0`) and that pulls `option-ext` 0.2.0, which is `MPL-2.0`. MPL-2.0 is not on the `deny.toml` allow-list, and adding a license needs an ADR first. Three path lookups do not justify that, so the resolver uses `std` only.

## Consequences

- Good: one small tested API for all three files; typo-safe; no new license.
- Good: `sqler-core` stays serde-free; `sqler-config` maps the file into `ConnectionConfig`.
- Bad: the directory lookup is hand-written and must be kept in line with platform conventions (covered by unit tests with a fake environment).
- Bad: no environment override of the config directory yet.
- Neutral: sequences of chords and context-dependent (`when`) bindings are not supported; both can be added to the schema later without breaking it.

## Pros and cons of the options

- `directories`: well known and handles more platforms, but brings an MPL-2.0 crate. Rejected.
- `std`-only lookup: three small rules, no dependency. Chosen.
- JSON or a custom format: JSON has no comments, which hurts hand-edited files; a custom format costs a parser. Rejected.
