# 0042 Workbench layout and editor groups

- Status: accepted
- Date: 2026-10-21

## Context and problem statement

The shell is a flat row: a sidebar and a column with one tab strip, an export bar and the body. There is no activity bar, no status bar, no bottom panel, no resizing, no editor groups and no layout persistence (see [Workbench design](../architecture/workbench.md), section 1). The product owner requires a VS Code / Azure Data Studio / Zed style workbench: activity bar, resizable and collapsible panels, a real tab bar with preview and pinned tabs, split editors, a bottom panel for results, messages and history, and a status bar. We must choose how to model and draw it in GPUI without bypassing the close path that guards pending edits, session loans and grid cancellation.

## Decision drivers

- Layout and tab logic is headless and unit-tested; views only render state (AGENTS.md section 2).
- Every action is a `Command` with an ID.
- Closing a tab always goes through `Workspace::request_close_tab` (prompts, session loan, grid cancel).
- A result grid, its edit flow (`GridKey{tab,result}`) and its session loan stay bound to the query tab.

## Considered options

1. `gpui-component::dock::DockArea` with panel entities.
2. A nested split tree of editor panes.
3. Own headless model: `WorkbenchLayout` plus `TabStrip` groups (at most 4, horizontal), drawn with `h_resizable`/`v_resizable`.
4. Results as editor tabs (moved out of the query tab).

## Decision outcome

Option 3. `WorkbenchLayout` (side panel visibility, width and view, bottom panel visibility, height and view, a generation counter; the view holds the current focus region) and `TabStrip` groups are plain Rust in `sqler-ui`. `TabStrip::active()` is the focused group's active tab, so existing callers keep working.

- **Activity bar, side panel, status bar.** A 44 px activity bar (Connections, Saved Queries, History; Settings at the bottom), a side panel of 180..600 px (default 260, collapsible), and a 24 px status bar with environment chip, connection, database, run state, rows and timing.
- **Editor groups.** One to four groups side by side with move-split semantics (`pane:split-right` moves the active tab into a new group, because an `InputState` or table entity renders in one place only). Groups are not persisted.
- **Preview and pinned tabs.** A browser object open (data, DDL, properties) is a preview tab and replaces the group's previous preview through the normal close path. It pins on tab double-click, `tab:pin`, a staged edit or a drag. Query editors are always pinned.
- **Bottom panel.** Results, Messages and History in a `v_resizable` panel (120 px to 70 % of the window, default 380). It follows the focused group's active tab. A run always reveals Results. `history:open` shows History in the panel.
- **Welcome.** An empty editor area shows a welcome view (actions with shortcut hints, recent connections), not a tab.
- **Settings tab** stays a special case rendered at the end of the first group's strip.
- **Owner questions (defaults taken, status stays accepted; each Q is open to owner override by follow-up):** Q1 results move into the shared bottom panel (the ADS alternative of keeping them in the editor was not chosen); Q2 `history:open` opens History in the panel and keeps its ID; Q5 browser object tabs open as preview.

The tab bar is our own element: preview, dirty, middle-click, drag and the environment top border are not in the `gpui-component` `TabBar`.

Rejected:

- `DockArea`: its panels are entities with their own close and serialize path, which bypasses `request_close_tab`, and it is not headless-testable.
- A nested split tree: more power than the owner asked for, harder to persist and test; four horizontal groups cover the use case.
- Results as editor tabs: the `GridModel`, the edit flow and the session loan are bound to the query tab.

## Consequences

- Q1, Q2 and Q5 change behaviour for existing users: results appear in the bottom panel, History is a panel view, browser opens replace a preview.
- `workspace/view.rs` render is rewritten by the shell item (U3); all `grid:*`, fetch-more, export and edit commands keep resolving `active_table()` through the focused group's query tab.
- Preview replacement must use the close path; previews are never dirty because edits pin them.
- Amendment (U3 review): the default panel height is 380 px (about 48 % of a 1280x800 window), up from 280. With results out of the editor, 280 px left room for two data rows under the panel header, one toolbar row, the filter bar and the column header; 380 px shows at least eight rows. A saved height is kept as is.
- Layout persistence is ADR-0045; keyboard and accessibility are ADR-0046.
