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
- **Not persisted.** Open tabs (tab restore is carried over to a later milestone) and editor groups.

Rejected:

- Keys in `settings.toml`: `deny_unknown_fields` breaks a downgrade.
- JSON: the repository convention for config is TOML.

## Consequences

- `Startup` gains a `workbench: WorkbenchStartup` field; `sqler-app` loads it.
- Budgets that the workbench items must meet are in the design page: cold start at most +3 ms per item and +5 ms for M4.5, idle RSS +5 MiB, binary +300 KiB, grid and browser benches green, theme parse and workbench read at most 1 ms each.
- A newer-format file left by a future version is preserved, at the cost of the layout not being saved by this version.
