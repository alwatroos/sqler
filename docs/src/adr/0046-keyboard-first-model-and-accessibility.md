# 0046 Keyboard-first model and accessibility

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

Keys are single-stroke chords routed by `WorkspaceView::on_key` capture and `KeyDispatcher`; labels print `ctrl+shift+p`. There is no region cycling, no tab-stop policy, no accessibility labels and no palette history. The new workbench adds many actions that need command IDs, defaults and a reachable keyboard path.

## Decision drivers

- Every action is a `Command` with an ID; existing IDs and defaults do not change; defaults stay conflict-free.
- A keyboard-only user can open a connection, run, read results and close a tab.
- Focus is always visible; colour is never the only signal.
- A prod write is still confirmed.

## Considered options

1. Add commands with single-stroke defaults, region cycling on F6, `tab_stop` controls.
2. Add multi-stroke chords (for example `mod+k mod+b`).

## Decision outcome

Option 1. Details are in [Workbench design](../architecture/workbench.md), section 4.

- **Commands.** New IDs with titles and defaults: `workbench:toggle-side-panel` (`mod+b`), `workbench:show-connections` (`mod+shift+e`), `workbench:toggle-bottom-panel` (`mod+j`), `workbench:focus-next-region` / `-previous-region` (`f6` / `shift+f6`), `pane:split-right` (`mod+\`), `tab:move-left` / `tab:move-right` (`mod+shift+pageup` / `pagedown`), plus the palette-only commands for panels, groups, tab operations and `query:explain`. Drag-driven commands take an argument (`NEEDS_ARGUMENT`).
- **Regions.** ActivityBar, SidePanel, Editor, Panel, StatusBar; F6 cycles them and skips hidden ones. Entering a region focuses its last-focused element, else its default. Inside a region, Tab and Shift+Tab move between `tab_stop(true)` controls; Enter and Space activate; Escape closes an overlay, or returns from the panel to the editor.
- **Accessibility.** `aria_label` and a role (Button, Tab, TabList) on every icon button, activity item, tab, panel tab and status item. Focus ring from the theme (`ring`). The PROD chip has text, a failed statement has an icon and text, dirty has the tooltip "Unsaved changes".
- **Shortcut display.** Labels use platform glyphs (`display_chord`; for example `⌘⇧P` on macOS, `Ctrl+Shift+P` elsewhere) in tooltips, menus and the palette.
- **Palette recents.** An empty query lists "Recently used" (up to 10 commands run from the palette, persisted per ADR-0045) before all commands. A typed query keeps today's ranking.
- **Explain.** `query:explain` wraps the statement at the cursor: `EXPLAIN ` for PostgreSQL, MySQL and DuckDB, `EXPLAIN QUERY PLAN ` for SQLite, a notice for SQL Server. It never adds `ANALYZE`. The text runs through the normal run pipeline, so the production gate classifies `EXPLAIN <write>` as the write and still confirms (ADR-0024).
- **Unsaved text (Q3, default taken; owner may overrule by follow-up).** Closing an editor with unsaved text does not prompt (decision: no prompt, unchanged behaviour); the pending-edits prompt stays. A prompt waits until tab restore exists.

Rejected: multi-stroke chords. They change the chord grammar and the keybinding file shape (ADR-0005), for no need the single-stroke defaults do not meet.

## Consequences

- `defaults_complete_and_conflict_free` must keep passing with the new defaults; a user override still replaces or unbinds them.
- `ids_are_valid` covers the new IDs; `object:show-menu` keeps its palette fallback because menus cannot open from code.
- Screen-reader behaviour (VoiceOver and others) is best effort and verified by the owner.
