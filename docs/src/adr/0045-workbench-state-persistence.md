# 0045 Workbench state persistence

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

Nothing about the layout survives a restart. The overhaul adds panel visibility and sizes, the active side and panel views, the window size, and recent commands and connections. `settings.toml` has `deny_unknown_fields`, so a newer key would stop an older binary from reading it. `keybindings.toml` and `connections.toml` are user configuration with their own grammar.

## Decision drivers

- Additive and versioned; older and newer binaries must not break each other's files.
- Existing config formats, command IDs and defaults are unchanged.
- No write per drag frame; a crash must not leave a half-written file.
- The repository convention for config is TOML with `read_optional` and `write_atomic`.

## Considered options

1. New keys in `settings.toml`.
2. A new `workbench.toml` in `sqler-config`.
3. JSON state file.

## Decision outcome

Option 2. `sqler-config/src/workbench.rs` defines `WorkbenchState` (`version = 1`, `[side_panel]`, `[bottom_panel]`, optional `[window]`, `recent_commands` up to 10 IDs, `recent_connections` up to 5 IDs) and `ConfigDir::{workbench_path, load_workbench_state, save_workbench_state}`. The full contract is in [Workbench design](../architecture/workbench.md).

- **Read is lenient.** Unknown keys are ignored; out-of-range or unknown values fall back per field to the default or the clamp. A missing file gives defaults.
- **Newer is read-only.** `version > 1` gives defaults and the file is never written (same rule as settings).
- **Invalid is backed up, then replaced.** An unparsable or unreadable file gives defaults and one notice. Before the first save over it, the file is copied to `workbench.toml.bak` (one backup; a later one replaces it). If the copy fails, nothing is overwritten and the save reports an error that the caller shows as a notice. A file from a newer version is never touched.
- **Writes.** `write_atomic` on the GPUI background executor, debounced 500 ms after the last layout change (never per drag frame), plus one synchronous write in the quit hook.
- **Unchanged.** `settings.toml`, `keybindings.toml`, `connections.toml`, history and saved-query files. Users only gain new commands and defaults; an override still replaces or unbinds them.
- **Not persisted in `workbench.toml`.** Open tabs and editor groups. Tabs are kept in `session.json` (see the amendment below); groups are not restored.

Rejected:

- Keys in `settings.toml`: `deny_unknown_fields` breaks a downgrade.
- JSON: the repository convention for config is TOML.

## Consequences

- `Startup` gains a `workbench: WorkbenchStartup` field; `sqler-app` loads it.
- Budgets that the workbench items must meet are in the design page: cold start at most +3 ms per item and +5 ms for M4.5, idle RSS +5 MiB, binary +300 KiB, grid and browser benches green, theme parse and workbench read at most 1 ms each.
- A newer-format file left by a future version is preserved, at the cost of the layout not being saved by this version.

## Amendment: session.json (open editor tabs)

Open editor tabs, their text and the unsent AI chat draft are kept in a second file, `session.json`, next to `workbench.toml`. `workbench.toml` and its format do not change; it still restores the panel layout, sizes and chat panel visibility and width.

Why a separate file:

- **Privacy.** It holds SQL text, so it is written with `write_atomic_private` (mode 0600 on Unix).
- **Size.** It can reach megabytes, while the `workbench.toml` read has a 1 ms budget.
- **Failure isolation.** A corrupt `session.json` never costs the layout.
- **JSON, not TOML.** JSON round-trips every string exactly (CRLF, lone CR, NUL, emoji); TOML multi-line strings may normalise line endings. `serde_json` is already a dependency of `sqler-config`.

Format, `version = 1`: `{"version":1,"active":0,"tabs":[{"kind":"query","connection":"<id>"|null,"driver":"postgres","text":"...","dirty":true}],"chat":{"draft":"..."}}`. Only the connection id and the driver id are stored, never a name, URL, password, token or key. The SQL text is stored verbatim, since the exact text must come back.

Rules:

- **Read is lenient.** Unknown keys and entries with an unknown `kind` are ignored; a field of the wrong type falls back to its default; `active` is mapped to the kept entry or becomes none. A missing file restores nothing.
- **Caps.** The file at most 4 MiB, 50 tabs, 1 MiB of text per tab, 64 KiB for the chat draft. Larger tabs are dropped (oversized text first, then the tabs past 50, then tabs from the end until the file fits) and one notice says so. A file over the cap is treated as invalid.
- **Newer is read-only.** `version > 1` restores nothing and the file is never written.
- **Invalid is backed up, then replaced.** A file that cannot be parsed restores nothing and gives one notice; before the first save over it, the raw bytes are copied to `session.json.bak` (also 0600). If the copy fails, nothing is overwritten. Error messages never contain file content.
- **Writes.** A checkpoint every 5 s on the background executor, only when the state changed, plus one synchronous write in the quit hook. Up to 5 s of typing can be lost on a hard crash; a clean quit loses nothing.
- **Lazy attach.** Restored tabs open without a session. Only the active tab connects at launch; any other tab connects when it is first activated, so a launch causes no connect storm, keychain prompts or production logins. Running a tab that is not yet connected reports that its connection is not available. A failed connect is retried only when the tab is activated again.
- **Missing connection.** If the saved connection no longer exists, the tab stays open with its text, no connection and a notice.
- **Not restored.** Grid, DDL, properties, history, AI session and agent tabs, results, cursor, selection and undo history, and editor groups (tabs are flattened into one group; preview tabs come back pinned). The AI chat transcript is not restored, because it holds result rows and the provider state cannot be resumed; only the unsent draft is, and it fills the composer of the next chat. The chat is never started or sent automatically.
- **Two instances.** The last writer wins, as with `workbench.toml`.
