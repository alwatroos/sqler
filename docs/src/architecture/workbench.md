# Workbench design (M4.5)

Design reference for the M4.5 UI/UX overhaul. It records how the shell works today, the target workbench, the visual system, the keyboard and accessibility model, persistence and budgets, and the screenshot strategy. The decisions are in ADR-0042 to ADR-0047. This page is design only: the build items (U1 to U7) deliver it step by step.

SQLer keeps its own headless workbench model and draws it with GPUI plus `gpui-component` 0.8.0 building blocks. Layout state (activity view, side panel, editor groups, bottom panel, focus region) is plain Rust in `sqler-ui`, unit-tested, and stored additively in a new versioned `workbench.toml`. The views only render that state, and every action is a `Command` with an ID. Rejected: `gpui-component::dock::DockArea`, because its panels have their own close/serialize path, which would bypass `Workspace::request_close_tab` (prompts, session loan, grid cancel), and it is not headless-testable.

## Assumptions
- No new key in `settings.toml` (it has `deny_unknown_fields`, so a newer key would break an older binary). Layout/recents go to `workbench.toml`.
- `ThemePreference` (`light`/`dark`/`system`) keeps its meaning. It now maps to SQLer Light/SQLer Dark instead of the gpui-component built-ins. No user theme files exist today. A `themes/` directory (Zed/gpui-component JSON via `ThemeRegistry`) is design-ready but deferred.
- Tabs are still not restored on restart (carried item). Only layout, window size and recents persist.
- No embedded fonts: system UI font and platform mono (size/cold-start). Alternative (IBM Plex, OFL) rejected for about 400 KB and font-load time.
- Native window title bar stays. A custom `TitleBar` was rejected because of Linux CSD/Windows control risk.
- "Pinned" in the owner's text = VS Code "permanent" (not preview). Sticky pin is a third state and stays out (later).
- A query result is not moved into an editor tab: its `GridModel`, edit flow (`GridKey{tab,result}`) and session loan are bound to the query tab. Results are tabbed in the bottom panel. Table data/DDL/properties/editors/history are tabs.

## Owner questions, defaults applied

The product questions are taken with the recommended defaults and implemented unless the owner answers otherwise; every answer below is a default taken, not an owner decision: the owner may overrule any of them by a follow-up (recorded in ADR-0042, ADR-0043 and ADR-0046, and listed as awaiting owner veto in the PR).

- Q1: Query results move from below each editor into the shared bottom panel, which follows the focused editor (VS Code/owner wording; ADS keeps them inside the editor). Decision: yes (default taken; owner may overrule by follow-up). Refinement: the batch close commands (Close Others, Close to the Right, Close All) treat an editor with unsaved text as pending changes and prompt for it like staged grid edits; the single-tab close is unchanged.
- Q2: `history:open` shows History in the bottom panel instead of opening an editor tab (ID unchanged). Decision: yes (default taken; owner may overrule by follow-up).
- Q3: Closing an editor with unsaved text (dirty dot) does NOT prompt (unchanged behaviour; the pending-edits prompt stays). Decision: no prompt (unchanged behaviour); a prompt waits until tab restore exists (default taken; owner may overrule by follow-up).
- Q4: When the focused tab's connection is `prod`, the whole status bar turns solid red (white text) in addition to the tab top border and PROD chips. Decision: yes (default taken; owner may overrule by follow-up).
- Q5: Object tabs opened from the browser (double-click/Enter: data, DDL, properties) open as preview and replace the previous preview. They pin on tab double-click, `tab:pin`, staged edit, drag. Query editors are always pinned. Decision: yes (default taken; owner may overrule by follow-up).

## 1. Current state
| Area | Today (paths) | Hard-coded / missing |
| ---- | ------------- | -------------------- |
| Shell | `workspace/view.rs` `Render` (l.1409): row of sidebar + column(tab strip + `settings::view::strip_end` + export bar + body); overlays (dialogs, palette) as children; window 1280x800 (l.133) | no activity bar, status bar, bottom panel, resize, persistence |
| State | `workspace/state.rs` `Workspace` (l.362) holds `TabStrip`, sessions, queries, editors, history, edit, exports; `dispatch` → `Effect` carried out by the view (`apply`, l.935) | settings tab is a special case `SettingsTab` (l.352), not a strip entry |
| Sidebar | `sidebar/model.rs` `SidebarModel` (groups, connections, selection); `sidebar/view.rs` `gpui::list` + `ListState`, object tree rows from `browser/tree.rs`, menus `browser/menu.rs`; saved queries section `history::view::saved_section` at the bottom | width `px(260.)` (l.283); chevrons are text glyphs "▸/▾" (l.449, 562, 684) |
| Tabs | `tabs/model.rs` `TabStrip` flat `Vec<Tab>`, append at end, close→right/left, `next/previous`; `tabs/header.rs` `TabHeader{title,accent,badge,is_prod}`; `tabs/view.rs` divs: 4 px accent stripe, "×" glyph, prod tint `danger.opacity(0.18/0.30)` | height 36; no overflow (clips), reorder, preview, dirty, middle-click, groups, context menu |
| Query tab | `query/view/mod.rs` editor `h(px(220.))` + `results.rs` `results_area` (statement chips, status line, Cancel, Fetch more) | fixed editor height; no toolbar/explain/breadcrumbs |
| Object tabs | `tabs/view.rs` `object_tab_body` (DDL text / properties rows, "Open in Editor", "Copy") | label column 180 px |
| Commands | `command/registry.rs` `CommandRegistry` (sorted `BTreeMap`); each module has `COMMANDS` + `register_commands` + `action_for`; `WorkspaceView::run_command` (view.rs l.586) chains them | — |
| Keys | `WorkspaceView::on_key` capture (l.386): overlay Escapes → `sidebar_key`/`grid_key` → `keymap::route_key`; `KeyDispatcher` over `sqler-config` `Keymap` (`DEFAULTS` keymap.rs l.229) + `keybindings.toml` overrides | single-stroke chords only; labels print `ctrl+shift+p` (no ⌘ glyphs) |
| Palette | `palette/mod.rs` `PaletteState{open,query,selected}`, matcher-ranked items with shortcut text; `palette/view.rs` overlay | no recents, no Kbd chips |
| Theme | `theme/mod.rs` `resolve(pref, system)`; `theme/apply.rs` `Theme::change(mode)` with gpui-component **built-in** light/dark + font size | no own palette; env badges reuse `danger`/`border` |
| Icons | none; no `AssetSource` registered in `run` | gpui-component's own icons (menus, notifications, tree) render blank |
| Sizes | 157 `px(..)`/`text_*` call sites in 14 view files (dialogs 520/620, label columns 120/180) | no spacing/type scale |
| Persistence | `settings.toml` (theme, ui_font_size, result_page_rows), `keybindings.toml`, `connections.toml`, history JSONL, saved queries TOML | no layout state |
| A11y | focus handles: root, sidebar, palette input, editors, grid tables; Up/Down tree walk | no region cycling, `tab_index`, `aria_label`, focus-ring policy |

## 2. Target workbench
```
┌──┬────────────────┬──────────────────────────────────────────────────┐
│A │ SIDE PANEL     │ tab bar (group 1)        ║ tab bar (group 2)     │32
│c │ header 30      │ breadcrumbs ⋯ toolbar  32║                       │
│t │ CONNECTIONS +  │ editor body              ║ editor body           │
│i │ tree / saved   ├──────────────────────────╨───────────────────────┤
│v │ queries        │ BOTTOM PANEL header 30: Results | Messages | History  ✕│
│44│ 180..600 (260) │ result strip / grid / list            120..70% (380) │
├──┴────────────────┴──────────────────────────────────────────────────┤
│ STATUS 24: [PROD] ● conn  ⛁ db            Running… 1.2 s · 1,234 rows · 38 ms  ▣ ▤ │
└──────────────────────────────────────────────────────────────────────┘
```
- **Activity bar** (44 px, VS Code/ADS): top items Connections (`Database`, `workbench:show-connections`), Saved Queries (`Bookmark`, `workbench:show-saved-queries`), History (`Clock`, `history:open` → panel). Bottom item: Settings (`Settings`, `settings:open`). An item is 44x40 with a 20 px icon. The active item has a 2 px left indicator. Clicking the active item toggles the side panel (VS Code). Every item has a tooltip with title + shortcut.
- **Side panel**: `h_resizable`. Width clamp 180..600, default 260, collapsible (`mod+b`). Header 30 px: uppercase `xs` muted title + icon buttons (Plus = `connection:new`, Folder = `connection-group:new`). The Connections view = today's sidebar list (connections + object trees). The Saved Queries view = today's saved section, full height.
- **Editor area**: 1..4 **editor groups** side by side (`h_resizable`, equal widths, not persisted). One group is focused, and `TabStrip::active()` = the focused group's active tab, so every existing caller keeps working.
- **Tab bar** (32 px, own element). Order: kind icon (14 px), title (truncated, min 80/max 220 px, full title in tooltip), env chip, then a trailing slot. The trailing slot shows the close `X` on hover/active, a dirty dot (8 px) when dirty and not hovered, and a spinner while running. Preview = italic title. Active tab = editor background and `tab.active.foreground`. Inactive = `tab.foreground` on `tab_bar`. 1 px `border` separators. Connection colour = 6 px dot before the title (replaces the 4 px stripe). Prod: active tab gets a 2 px top border `env.prod.solid` and a solid PROD chip; inactive prod tabs show the chip only.
  Interactions: click activates. Middle-click (`MouseButton::Middle`) closes through `CloseTab`. Double-click pins. Drag reorders inside a group or moves to another group's strip (gpui `on_drag`/`on_drop`, pattern of `DragConnection` in `sidebar/view.rs`). Right-click opens a menu: Close, Close Others, Close to the Right, Close All, Pin, Split Right, Move to Next Group, Copy Title.
  Overflow: the strip scrolls horizontally (`overflow_x_scroll` + `ScrollHandle::scroll_to_item(active)`), and a trailing `ChevronDown` dropdown lists all tabs of the group. The settings tab stays special and renders at the end of the first group's strip.
- **Open/close rules**: `open`/`open_object` append to the focused group as pinned (unchanged). Browser object opens use preview: they replace the group's preview tab in place, closing it through the normal close path. Closing the active tab → right, else left neighbour in its group. An emptied group is removed (unless it is the last), and focus goes to the left neighbour group. `tab:next/previous` cycle inside the focused group.
- **Split**: `pane:split-right` moves the active tab into a new group right of the focused one. It is refused with a notice when the group has one tab or 4 groups exist. Move semantics are used because an `InputState`/table entity can render in one place only.
- **Bottom panel** (`v_resizable` under the editor area; clamp 120..70 % of window, default 380, `mod+j`). It has three views and follows the focused group's active tab:
  - Results: today's statement chips restyled as segmented tabs with a status icon, the paused/fetch-more line, and the grid.
  - Messages: one line per statement: title, status text, duration, error in `danger`.
  - History: the single panel-hosted History tab, bound to the active connection.
  A run always reveals Results. A non-query active tab shows the empty state "No results for this tab". Escape in the panel returns focus to the editor.
- **Query editor tab body**: a toolbar row (32 px), then the editor at full height. The toolbar holds breadcrumbs `conn › database` + env chip on the left. On the right: Run (`Play`, primary small, `query:run-statement`), Run All (`SquarePlay`), Cancel (`CircleStop`, only enabled while running), Explain (`ListTree`, `query:explain`), Format (`WandSparkles`), Save (`Save`, `query:save`). The DDL/Properties tab toolbar has breadcrumbs `conn › schema › object` + Open in Editor + Copy. The data tab toolbar has breadcrumbs + the existing edit bar.
- **Status bar** (24 px, `status_bar` colours, `xs` text, items 8 px padding, hover bg).
  - Left: env chip, connection dot + name, database (`Database` icon + `database_label`).
  - Right: run indicator ("Running…"/"Cancelling…" + `LoaderCircle`), rows (`format_rows`), timing of the shown statement, two toggle buttons (`PanelLeft`, `PanelBottom`).
  - Prod: the bar turns `env.prod.solid` with `on_solid` text. Clicking the connection item runs `workbench:show-connections`.
- **Welcome / empty editor area** (Zed-style empty pane, not a tab). Title "SQLer" `xl` semibold.
  - Left column: actions, each with a Kbd hint: New Connection, New SQL Editor (disabled without a selected connection, with a tooltip saying why), Open Saved Query, Show All Commands.
  - Right column: "Recent connections" (up to 5, click = open).
  - With no connections at all, the empty state is one primary button "Add your first connection".

## 3. Visual system
- **Spacing** (px): `SPACE = [2, 4, 6, 8, 12, 16, 24, 32]`. Paddings: rows 4/8, panels 8/12, dialogs 16/24. Gaps 4/6/8.
- **Sizes**: activity 44, tab bar 32, toolbar 32, panel header 30, status 24, list row 24, icon 14 (inline) / 16 (buttons) / 20 (activity), hit target ≥ 24.
- **Typography** (system UI font; `ui_font_size` = U, default 14): xs U-3, sm U-1, md U, lg U+2, xl U+6, mono U-1 (editor/grid). Weights 400 body, 500 active tab/section headers, 600 welcome title. Section headers are uppercase `xs` with +0.04em tracking.
- **Shape**: radius sm 4 / md 6 (buttons, inputs, chips) / lg 8 (dialogs, popovers, palette). Borders are 1 px `border`, never boxed cards. `shadow: false` (flat, Zed-like). Overlays get `border` + `overlay` scrim.
- **States**:
  - hover = `list.hover.background` (rows, ghost buttons, tabs, status items)
  - pressed = `list.active.background`
  - selected+focused = `list.active.background` + `list.active.border` left 2 px
  - selected unfocused = hover colour
  - focus = 1 px `ring` (gpui-component `focus_ring = true`)
  - disabled = 50 % opacity, no hover, `muted.foreground`
- **Icons**: Lucide 1.43 SVGs from the `gpui-kit-assets` 0.8.0 crate (Apache-2.0 crate; icons ISC, Feather-derived MIT). They are embedded with its `icon_assets!` macro, selected set only (42 SVGs, about +20 KiB), and composed with `gpui_kit_assets::Assets` (the 106 component icons, which fixes today's blank component icons). The licence text is copied to `licenses/LUCIDE-ISC.txt` and listed in `docs/src/dependencies.md`.
  The set (`AppIcon` → `IconName`): Database, Server, Table, Table2, Eye, KeyRound, SquareFunction, Folder, FolderOpen, Play, SquarePlay, CircleStop, ListTree, FileCode, FileText, SlidersHorizontal, Clock, Bookmark, Settings, Search, X, Plus, ChevronRight, ChevronDown, PanelLeft, PanelBottom, Columns2, Pin, MessageSquare, WandSparkles, Save, RefreshCw, Copy, Plug, Unplug, TriangleAlert, CircleAlert, Info, Check, Ellipsis, LoaderCircle, Command.
- **Theme tokens** live in `crates/sqler-ui/assets/themes/sqler.json`, a gpui-component `ThemeSet` with two `ThemeConfig`s (`radius` 6, `radius.lg` 8, `shadow` false, no font keys):

| key | SQLer Dark | SQLer Light |
| --- | --- | --- |
| background / foreground | #282c33 / #dce0e5 | #fafafa / #242529 |
| border / input.border / ring | #363c46 / #464b57 / #74ade8 | #dfdfe0 / #c9c9ca / #4a65d1 |
| muted.background / muted.foreground | #2f343e / #a9afbc | #ebebec / #58585a |
| accent.background / accent.foreground | #363c46 / #dce0e5 | #dfdfe0 / #242529 |
| primary.background / .foreground / .hover / .active | #74ade8 / #1a1d23 / #8bbcec / #5e9bd9 | #4a65d1 / #ffffff / #3f58bd / #364ca6 |
| secondary.background / .foreground | #363c46 / #dce0e5 | #ebebec / #242529 |
| popover.background / .foreground | #2f343e / #dce0e5 | #ffffff / #242529 |
| sidebar.background / .foreground / .border | #2f343e / #dce0e5 / #363c46 | #ebebec / #242529 / #dfdfe0 |
| sidebar.accent.background / .foreground | #454a56 / #ffffff | #d3d3d4 / #111214 |
| list.background / list.hover.background | #00000000 / #363c46 | #00000000 / #dfdfe0 |
| list.active.background / list.active.border | #74ade82e / #74ade8 | #4a65d126 / #4a65d1 |
| tab_bar.background / tab.background / tab.foreground | #2f343e / #2f343e / #a9afbc | #ebebec / #ebebec / #58585a |
| tab.active.background / tab.active.foreground | #282c33 / #dce0e5 | #fafafa / #242529 |
| status_bar.background / status_bar.border | #2f343e / #363c46 | #ebebec / #dfdfe0 |
| table.background / table.head.background / table.head.foreground | #282c33 / #2f343e / #a9afbc | #fafafa / #f3f3f4 / #58585a |
| table.even / table.hover / table.row.border | #2b2f36 / #31363f / #2f343e | #f5f5f6 / #efeff0 / #ebebec |
| table.active.background / .border, selection.background, caret | #74ade83d / #74ade8, #74ade83d, #74ade8 | #4a65d133 / #4a65d1, #4a65d133, #4a65d1 |
| scrollbar.background / thumb / thumb.hover | #00000000 / #c8ccd44c / #c8ccd480 | #00000000 / #24252940 / #24252966 |
| danger.background / .foreground / .hover | #d07277 / #1a1d23 / #da878b | #c62828 / #ffffff / #b71c1c |
| warning.background / .foreground | #dec184 / #1a1d23 | #8a6d00 / #ffffff |
| success.background / .foreground | #a1c181 / #1a1d23 | #3f7d32 / #ffffff |
| info.background / .foreground | #74ade8 / #1a1d23 | #4a65d1 / #ffffff |
| drag.border / drop_target.background / overlay | #74ade8 / #74ade826 / #00000066 | #4a65d1 / #4a65d11f / #00000033 |
| highlight editor.background / .foreground / .active_line.background | #282c33 / #dce0e5 / #2f343ebf | #fafafa / #242529 / #ebebec80 |
| highlight editor.line_number / .active_line_number | #636b78 / #dce0e5 | #929498 / #242529 |
| syntax keyword, string, number+boolean, comment | #b477cf, #a1c181, #bf956a, #5d636f italic | #a449ab, #4d7f3c, #ad6e25, #8e9096 italic |
| syntax function, type+operator, punctuation, constant, variable | #73ade9, #6eb4bf, #acb2be, #dfc184, #dce0e5 | #4a65d1, #2f7aa8, #4d4f52, #a16b00, #242529 |

- **SQLer-only tokens** (`theme/tokens.rs`, Rust, headless): activity bar bg = sidebar bg; activity fg #878a98 / #7e7f83; active fg = foreground; indicator = ring; dirty dot = muted.foreground; preview = tab.foreground + italic.
- **Environment accents**. Chip = `fg` text on `fg` at alpha 0.18; prod chip = `solid` + `on_solid`. The connection colour stays a dot and never replaces the env colour.

| env | dark fg | light fg | solid / on_solid (prod only) |
| --- | --- | --- | --- |
| dev | #a1c181 | #3f7d32 | — |
| staging | #dec184 | #8a6d00 | — |
| prod | #e06c75 | #c62828 | #b83a3a / #ffffff (dark), #c62828 / #ffffff (light) |

- **Contrast rule** (headless test): foreground, muted.foreground, tab.foreground, tab.active.foreground, sidebar.foreground and status text on their backgrounds, primary/danger/success/warning/info foreground on their fills, and prod `on_solid` on `solid` must all be ≥ 4.5:1 in both themes.
- **Compatibility**: `theme`, `theme:toggle-mode`, `settings:set-theme`, `settings:set-ui-font-size` are unchanged. `apply` installs SQLer themes as `Theme.light_theme`/`dark_theme` before `Theme::change(mode)` and keeps the font-size override after it. The look of light/dark/system changes for everybody (intended).

## 4. Intuitiveness, keyboard and accessibility
- **Toolbars**: see §2. Every icon-only button has a tooltip `Title (⌘↩)`. Disabled buttons explain why in the tooltip ("Select a connection first").
- **Context menus** (`ContextMenuExt`; items built with `PopupMenuItem::element` = label + right-aligned muted shortcut): tab bar (§2), connection rows / object rows / grid (existing entries, add shortcut hints), panel header (Hide Panel, Move Results here). `object:show-menu` keeps its palette fallback (gotcha: menus cannot open from code).
- **Empty states** (icon 20 muted + title `md` 500 + one line `sm` muted + primary action): welcome; Connections empty ("No connections yet" + New Connection); Saved Queries ("No saved queries. Save the editor text with Query: Save Query"); Results ("Run a query to see results here" + Kbd ⌘↩); Messages ("No messages yet"); History ("No history for this connection yet").
- **Palette**: rows show `Kbd`-style chips from `shortcut_display` (⌘⇧P on macOS, Ctrl+Shift+P elsewhere). An empty query lists "Recently used" (up to 10 MRU commands run from the palette, persisted) with a section label, then all commands. A typed query keeps today's ranking.
- **Breadcrumbs**: `gpui-component` `Breadcrumb`. Items are text, not links (no navigation in M4.5).
- **New commands** (IDs pinned; `NEEDS_ARGUMENT` gets the ones marked *arg*):

| ID | Title | Default |
| --- | --- | --- |
| `workbench:toggle-side-panel` | View: Toggle Side Panel | `mod+b` |
| `workbench:show-connections` | View: Show Connections | `mod+shift+e` |
| `workbench:show-saved-queries` | View: Show Saved Queries | — |
| `workbench:toggle-bottom-panel` | View: Toggle Panel | `mod+j` |
| `workbench:show-results` / `workbench:show-messages` | View: Show Results / Show Messages | — |
| `workbench:focus-next-region` / `-previous-region` | View: Focus Next / Previous Region | `f6` / `shift+f6` |
| `workbench:reset-layout` | View: Reset Layout | — |
| `workbench:set-side-width` / `workbench:set-panel-height` *arg* | (drag) | — |
| `pane:split-right` | View: Split Editor Right | `mod+\` |
| `pane:focus-next` / `pane:focus-previous` | View: Focus Next / Previous Editor Group | — |
| `pane:move-tab-to-next` / `pane:move-tab-to-previous` | View: Move Tab to Next / Previous Group | — |
| `pane:focus` *arg* | (click in a group) | — |
| `tab:pin` | Pin Tab | — |
| `tab:close-others` / `tab:close-to-the-right` / `tab:close-all` | Close Other Tabs / Close Tabs to the Right / Close All Tabs | — |
| `tab:move-left` / `tab:move-right` | Move Tab Left / Right | `mod+shift+pageup` / `mod+shift+pagedown` |
| `tab:move` *arg* | (drag) | — |
| `query:explain` | Query: Explain Statement at Cursor | — |

  Existing IDs and their defaults are unchanged, and the defaults stay conflict-free (`defaults_complete_and_conflict_free`).
  `query:explain` wraps the statement at the cursor with `explain_text(dialect, sql)`: `EXPLAIN ` for `PostgreSQL`/`MySQL`/`DuckDB`, `EXPLAIN QUERY PLAN ` for `SQLite`, and `None` (refused with a notice) for SQL Server. It never adds `ANALYZE`. The text then runs through the normal run pipeline, so the prod gate classifies `EXPLAIN <write>` as the write and still confirms.
- **Keyboard model**: the five regions are ActivityBar → SidePanel → Editor (focused group) → Panel → StatusBar. F6/Shift+F6 cycle them and skip hidden ones (`next_region`). Entering a region focuses its last-focused element, else its default: activity = active item, side = `sidebar_focus`, editor = active editor or table, panel = shown table or list, status = first item. Inside a region, Tab/Shift+Tab move between `tab_stop(true)` controls (activity items, toolbar buttons, panel tabs, welcome actions, status items). Enter/Space activate. Escape: overlay → close, panel → back to the editor.
- **Accessibility**: every icon button, activity item, tab, panel tab and status item gets `aria_label` (+ accesskit `role` Button/Tab/TabList). Focus is always visible (`ring`). Colour is never the only signal: the PROD chip has text, a failed statement has an icon + text, dirty has a tooltip "Unsaved changes".

## 5. Persistence, compatibility and budgets
- **`workbench.toml`** (config dir, `sqler-config`):
  - Contents: `version = 1`; `[side_panel]` visible/size/view (`connections|saved-queries`); `[bottom_panel]` visible/size/view (`results|messages|history`); optional `[window]` width/height; `recent_commands` (≤10 IDs); `recent_connections` (≤5 connection IDs).
  - Reading is lenient: unknown keys are ignored, and out-of-range or unknown values fall back per field to the default/clamp.
  - A missing file gives defaults.
  - `version > 1` gives defaults, and the file is **never written** (read-only, same rule as settings).
  - An unparsable file gives defaults + one notice, and the next save replaces it (it is app state, not user config).
  - Writes use `write_atomic` on the GPUI background executor: debounced 500 ms after the last layout change (never per drag frame), plus one synchronous write in the quit hook.
- **Unchanged**: `settings.toml`, `keybindings.toml` (grammar, file shape, every existing ID/default), `connections.toml`, history and saved-query files. Users only gain new commands/defaults; an override still replaces or unbinds them.
- **Budgets** (measured in U1, U2, U3, U7 PRs on macOS, `hyperfine --warmup 3 --runs 20 'target/release/sqler --exit-after-first-frame'` with a scratch `HOME`, so the run does not load the user's config):
  - cold start ≤ +3 ms per item and ≤ +5 ms for M4.5 vs `1ea7c39`
  - idle RSS ≤ +5 MiB
  - binary ≤ +300 KiB total (themes ~10 KiB, icons ~20-60 KiB; report `size.yml`)
  - `grid_viewport`, `grid_sort`, `browser_tree` gates green (grid styling must not touch `grid/model.rs`, `viewport.rs`, `order.rs`)
  - theme JSON parse + workbench read ≤ 1 ms each (a measurement target, not a unit-test gate; the `parsing_is_fast` test only asserts a 50 ms ceiling)

## 6. Screenshot strategy
- **Harness**: `crates/sqler-ui/tests/screenshots.rs`, `harness = false`, `required-features = ["screenshots"]`, feature `screenshots = ["gui", "gpui/test-support", "gpui_platform/test-support"]`. It runs on the main thread and uses `gpui::HeadlessAppContext::with_platform(gpui_platform::current_platform(true).text_system(), Arc::new(()), gpui_platform::current_headless_renderer)`. `()` is GPUI's empty asset source: U6 ships no `AppAssets` (it merges before U1 and must not depend on it), so the "before" set shows today's look including the blank icons. U1 switches this one line to `AppAssets`.
- **Capture loop**: open a 1280x800 window with `Root` + `WorkspaceView` from a scene script, then call `run_until_parked`, `update_window(.., |_, w, cx| w.draw(cx))`, `capture_screenshot(handle)` and save `PNG` (dev-dep `image` with `png`, same version as the lockfile's 0.25.10, checked on crates.io).
- **Output and checks**: files go to `$SQLER_SCREENSHOT_DIR` (default `target/screenshots/`) as `<scene>-<dark|light>.png`. A scene fails if the image has the wrong size or fewer than 16 distinct colours. Optional layout checks use `window.painted_quads()`.
- **Renderers**: macOS = Metal headless. Linux = `WgpuHeadlessRenderer` on lavapipe (`mesa-vulkan-drivers libvulkan1 fonts-dejavu-core`). Windows = no headless renderer, so the harness prints `skipped: no headless renderer on windows` and exits 0.
- **Scenes**: `welcome-empty`, `welcome-recent` (3 connections dev/staging/prod), `editor-results` (SQLite `:memory:` via a test `Connector` built on the `sqler-driver-sqlite` dev-dep; fallback `dev:open-synthetic-grid`), `prod-editor`, `tabs-overflow` (14 tabs: preview, dirty, prod), `split-editors`, `object-browser-ddl`, `palette-recent`, `messages-error`, `settings`, `panels-collapsed`, `connection-dialog`. Each is rendered in both themes. Menus, hover and drag cannot be scripted, so they go to the checklist.
- **CI**: new `screenshots` job in `ci.yml` (Linux, required) runs `cargo test -p sqler-ui --features screenshots --test screenshots` and uploads `target/screenshots` as an artifact. A macOS job with `continue-on-error: true` runs the same (paravirtual Metal is not guaranteed).
- **PRs and report**: the tester runs the harness on macOS and commits the PNGs of the scenes the item changes to `docs/src/screenshots/<scene>-<theme>.png` (overwritten per item; U6 also commits `docs/src/screenshots/before/` from the pre-overhaul UI). PR bodies embed `https://github.com/alwatroos/sqler/raw/<head-sha>/docs/src/screenshots/<file>.png`. The book page `docs/src/user-guide/workspace.md` and the iteration report link the same files. Rejected: an orphan `screenshots` branch, which needs extra push handling and keeps the images out of the book.
- **Interactive checklist for the owner** (goes into every PR's "Pending (maintainer)" list where relevant):
  1. Drag-resize the side panel and the bottom panel, with min/max clamps; collapse both, restart, and check the layout and window size are restored.
  2. Tab hover shows the close button; middle-click closes; drag reorders and moves a tab to the other group; double-click pins; a preview tab is replaced by the next double-click in the browser; overflow scroll by trackpad and the overflow list.
  3. Typing shows the dirty dot; Save Query clears it; the tab context menu entries work.
  4. F6/Shift+F6 cycle the regions with a visible focus ring; a keyboard-only walk works: open a connection, run, read results, close a tab.
  5. Tooltips show platform shortcut glyphs; the palette "Recently used" list survives a restart.
  6. The prod status bar and chips stand out; the prod confirmation still triggers from the toolbar Run and Explain.
  7. Icon crispness on HiDPI; both themes on macOS, Linux (X11 and Wayland) and Windows; VoiceOver reads the activity bar and tab labels (best effort).

## Contracts

Rust names are pinned. Implementation items may add private helpers, not rename these.
```rust
// sqler-config/src/workbench.rs (U2)
pub const WORKBENCH_VERSION: u32 = 1;
#[serde(default)] pub struct WorkbenchState { pub version: u32, pub side_panel: PanelState, pub bottom_panel: PanelState,
    pub window: Option<WindowState>, pub recent_commands: Vec<String>, pub recent_connections: Vec<String> }
pub struct PanelState { pub visible: bool, pub size: f32, pub view: String }   // defaults: (true,260,"connections"), (false,380,"results")
pub struct WindowState { pub width: f32, pub height: f32 }
pub enum WorkbenchLoad { Missing, Loaded(WorkbenchState), Newer(u32), Invalid(String) }
impl ConfigDir { pub fn workbench_path(&self) -> PathBuf; pub fn load_workbench_state(&self) -> WorkbenchLoad;
                 pub fn save_workbench_state(&self, s: &WorkbenchState) -> Result<(), ConfigError>; }
```
```rust
// sqler-ui/src/workbench/ (U2, headless)
pub enum SideView { Connections, SavedQueries }   pub enum PanelView { Results, Messages, History }
pub enum Region { ActivityBar, SidePanel, Editor, Panel, StatusBar }
pub enum WorkbenchAction { ToggleSidePanel, ShowSideView(SideView), ToggleBottomPanel, ShowPanelView(PanelView),
    SetSideWidth(u16), SetPanelHeight(u16), FocusNextRegion, FocusPreviousRegion, ResetLayout }
pub struct WorkbenchLayout { /* side_visible, side_width, side_view, panel_visible, panel_height, panel_view, generation */ }
impl WorkbenchLayout { pub fn from_state(s: &WorkbenchState) -> Self; pub fn apply(&mut self, a: WorkbenchAction) -> bool;
    pub fn reveal_panel(&mut self, v: PanelView); pub fn generation(&self) -> u64; /* + getters */ }
pub fn next_region(current: Region, layout: &WorkbenchLayout, forward: bool) -> Region;
pub struct RecentCommands; // record(&CommandId), iter(), from_strings(&[String], &CommandRegistry), to_strings(); cap 10
pub struct RecentConnections; // record(&ConnectionId), iter(), cap 5
pub struct WorkbenchStartup { pub dir: Option<ConfigDir>, pub load: WorkbenchLoad }   // new field `Startup::workbench`
pub struct StatusBarModel { pub env: Option<Environment>, pub connection: Option<String>, pub accent: Option<Rgb>,
    pub database: Option<String>, pub run: RunIndicator, pub rows: Option<String>, pub timing: Option<String> }
pub enum RunIndicator { Idle, Running, Cancelling }   pub struct RowInfo { pub loaded: u64, pub shown: u64, pub more: bool }
pub fn format_rows(info: RowInfo) -> String;          // "1 row", "1,234 rows", "500 of 1,234 rows", "100,000+ rows"
pub fn database_label(driver: &DriverId, target: &str) -> Option<String>; // URL path / file name / "memory"; never user, host, password
pub fn register_commands(r: &mut CommandRegistry) -> Result<(), CommandError>;  pub fn action_for(id: &CommandId) -> Option<WorkbenchAction>;
pub const SIDE_MIN: f32 = 180.; SIDE_MAX = 600.; SIDE_DEFAULT = 260.; PANEL_MIN = 120.; PANEL_DEFAULT = 380.; PANEL_MAX_FRACTION = 0.7;
```
```rust
// sqler-ui/src/tabs/ (U2): additive, existing signatures unchanged
pub struct GroupId(u32);  pub enum TabMode { Preview, Pinned }  pub const MAX_GROUPS: usize = 4;
pub struct Group { /* id, tabs: Vec<TabId>, active */ }  // fn id(), tabs(), active()
impl TabStrip { pub fn groups(&self) -> &[Group]; pub fn focused_group(&self) -> GroupId; pub fn focus_group(&mut self, g: GroupId) -> bool;
    pub fn group_of(&self, id: TabId) -> Option<GroupId>; pub fn active_in(&self, g: GroupId) -> Option<&Tab>;
    pub fn preview_in(&self, g: GroupId) -> Option<TabId>; pub fn mode(&self, id: TabId) -> Option<TabMode>; pub fn pin(&mut self, id: TabId) -> bool;
    pub fn open_preview(&mut self, c: TabConnection, kind: TabKind, object: impl Into<String>, at: Option<usize>) -> TabId;
    pub fn move_tab(&mut self, id: TabId, to: GroupId, index: usize) -> bool; pub fn split_right(&mut self) -> Option<GroupId>; }
pub struct TabHeader { /* existing fields */ pub kind: TabKind, pub preview: bool }
pub struct TabDecor { pub header: TabHeader, pub dirty: bool, pub running: bool }
// Workspace (U2): with_workbench(self, &WorkbenchLoad) -> Self; layout(); dispatch_workbench(WorkbenchAction) -> Effect;
// workbench_state(&self, &RecentCommands, Option<WindowState>) -> Option<WorkbenchState> /* None when Newer */;
// recent_connections(); tab_decor(TabId) -> Option<TabDecor>; status_bar(Option<RowInfo>) -> StatusBarModel; editor_edited(TabId)
// WorkspaceAction (U2) adds: PinTab(TabId), CloseOtherTabs(TabId), CloseTabsToRight(TabId), CloseAllTabs,
//   MoveTab{id: TabId, group: GroupId, index: usize}, MoveActiveTabLeft, MoveActiveTabRight, SplitRight,
//   FocusGroup(GroupId), FocusNextGroup, FocusPreviousGroup, MoveActiveTabToNextGroup, MoveActiveTabToPreviousGroup
// PaletteState (U2): recent(), set_recent(RecentCommands), record(&CommandId); PaletteItem gains `pub recent: bool`
```
```rust
// sqler-ui/src/theme/ (U1)
pub const SPACE: [f32; 8] = [2., 4., 6., 8., 12., 16., 24., 32.];
pub struct TypeScale { pub xs: f32, pub sm: f32, pub md: f32, pub lg: f32, pub xl: f32, pub mono: f32 } pub fn type_scale(ui: u16) -> TypeScale;
pub struct EnvAccent { pub fg: Rgb, pub chip_alpha: f32, pub solid: Option<(Rgb, Rgb)> } pub fn env_accent(e: Environment, a: Appearance) -> EnvAccent;
pub fn contrast(a: Rgb, b: Rgb) -> f32;  pub const THEMES_JSON: &str; // include_str!("../../assets/themes/sqler.json")
// gui: theme::icons::{AppAssets, AppIcon}; theme::widgets::{icon_button, toolbar_button, env_chip, kbd, section_header, empty_state}
// keymap (U1): pub fn display_chord(chord: &KeyChord, p: Platform) -> String; KeyDispatcher::shortcut_display(&self, id) -> Option<String>
// query (U4): pub fn explain_text(dialect: Dialect, sql: &str) -> Option<String>
```

## Implementation items and merge order

| Item | Wave | Depends | Owns (shared files noted below) | Delivers |
| ---- | ---- | ------- | -------------------------------- | -------- |
| U2 Workbench state | A | D1 | `crates/sqler-config/src/{workbench.rs,lib.rs,paths.rs,keymap.rs}`, `crates/sqler-ui/src/workbench/**` (new: mod, layout, recent, status, action), `src/lib.rs`, `src/tabs/{model.rs,header.rs,mod.rs}`, `src/workspace/{action.rs,state.rs,editor.rs,browser.rs,query.rs,history.rs}`, `src/editor/doc.rs`, `src/palette/mod.rs`, `src/keymap/route.rs`, `crates/sqler-app/src/main.rs` | headless layout/groups/preview/dirty/recents/status model, new commands + defaults, `workbench.toml`; no view change |
| U1 Visual system | A | D1 | `crates/sqler-ui/src/theme/**` (tokens, apply, icons, widgets), `crates/sqler-ui/assets/**`, `crates/sqler-ui/Cargo.toml` (`gpui-kit-assets` dep), root `Cargo.toml`, `src/keymap/{chord.rs,dispatcher.rs}`, `workspace/view.rs` **only** the `run()` `with_assets(AppAssets)` line, `licenses/LUCIDE-ISC.txt`, `docs/src/dependencies.md` | SQLer Dark/Light applied, icons render (incl. component icons), primitives, contrast test, Kbd glyph labels |
| U6 Screenshot harness | A | D1 | `crates/sqler-ui/tests/screenshots.rs`, `src/workspace/view/scene.rs` (+ `mod scene` line in view.rs, gated re-export in `workspace/mod.rs`), `[features]`/`[[test]]` + dev-dep `image` in `crates/sqler-ui/Cargo.toml`, `.github/workflows/ci.yml` (job), `docs/src/screenshots/before/**` | harness, CI artifact, "before" set |
| U3 Workbench shell | B | U1, U2, U6 | `workspace/view.rs` (render, on_key regions, persistence glue), `workspace/view/{shell.rs,activity_bar.rs,status_bar.rs,panel.rs}` (new), `tabs/view.rs`, `query/view/mod.rs` (split into `query_editor_body` + `query_results_panel`), `editor/view.rs` (dirty wiring), `workbench/messages.rs` (new), `docs/src/user-guide/workspace.md`, `docs/src/user-guide/keybindings.md` | activity bar, resizable/collapsible panels, groups + new tab bar, bottom panel (Results/Messages/History), status bar, welcome, F6 regions |
| U5 Component restyle | B | U1, U2 | `sidebar/view.rs` (fill parent, icons, rows), `connections/view.rs`, `palette/view.rs`, `settings/view.rs`, `grid/view.rs` + `grid/detail_view.rs` + `grid/edit/view.rs` (styling only), `export/view.rs`, `query/view/dialogs.rs`, `workspace/drop_dialog.rs`, `history/view.rs`, `browser/menu.rs` (shortcut hints) | tokens/icons/empty states/menus with hints across the existing components; palette "Recently used" rendering |
| U4 Query surface | C | U3 | `query/**` (toolbar.rs new, explain.rs new, action.rs, view/mod.rs, view/results.rs), `tabs/view.rs` object/data toolbars + breadcrumbs, `docs/src/user-guide/running-queries.md` | toolbar Run/Run All/Cancel/Explain/Format/Save, breadcrumbs, `query:explain`, segmented result strip |
| U7 Metrics + report | C | all | `docs/reports/iteration-005b.md`, `docs/src/benchmarks.md`, `AGENTS.md` §9, `CHANGELOG.md` | cold start/RSS/size/bench numbers, final screenshot set, owner checklist |

- **Shared files in wave A**: U1 and U6 both edit `crates/sqler-ui/Cargo.toml` (U1 the `gpui-kit-assets` dependency, U6 `[features]`, `[[test]]` and the `image` dev-dependency) and `workspace/view.rs` (U1 the `run()` assets line, U6 the `mod scene` line); U2 also touches `view.rs`. Each item adds only its own lines, so the edits are disjoint and the merge order below resolves any conflict; the item that lands second rebases and expects it.
- **Merge order**: D1 → U2 → U6 → U1 → U3 → U5 → U4 → U7. U6 lands before U1 so its "before" set shows the old look. U6 uses an empty asset source; U1 then switches the harness to `AppAssets` (one line). U3 before U5, because U3 resizes the sidebar container and U5 makes the sidebar fill it.
- **Parallelism (≤ 4 building worktrees)**: wave A runs U2, U1, U6 at once (3); wave B runs U3, U5 (2); wave C runs U4, then U7 alone. Each item still carries its own CHANGELOG line, docs, "Pending (maintainer)" list, screenshots from U6 onward, and bench/size numbers where listed in §5.
