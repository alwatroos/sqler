# UI workspace

The M1 shell lives in `sqler-ui` and the `sqler-app` binary. Decisions: [ADR-0001](../adr/0001-ui-framework-spike-plan.md) (GPUI) and [ADR-0007](../adr/0007-desktop-binary-and-ui-tokio-bridge.md) (binary, `gui` feature, bridge).

## State and views

State is plain Rust and has no GPUI types:

| Module | Types | Role |
| ------ | ----- | ---- |
| `sidebar` | `SidebarModel`, `SidebarEntry`, `SidebarRow`, `ConnectionStatus` | the `ConnectionStore` (nested groups) as rows, collapse state, selection of a connection or group, status by connection ID, load and save error; the index API of M1 is kept |
| `connections` | `Connector`, `ConnectRequest`, `SecretSource`, `ConnectFailure`, `HostKeyPrompt`, `ConnectionForm`, `Dialog`, `ConnectionAction` | the connect contract, the headless form state machine (validation, secret plan, test status), dialog state and the `connection:*` commands |
| `tabs` | `TabStrip`, `Tab`, `TabId`, `TabKind`, `TabConnection`, `TabHeader`, `Rgb` | tabs bound to one connection each; titles `name`, `name (2)`; header data (title, accent, badge, prod flag) |
| `theme` | `Appearance`, `resolve`, `toggled` | turns `ThemePreference` and the OS appearance into light or dark; `toggled` gives the explicit opposite |
| `settings` | `SettingsModel` | the `Settings` of `sqler-config`: checked changes (theme, font size 8 to 48), saved only through `ConfigDir::save_settings`; defaults and no writes when loading failed |
| `keymap` | `KeyInput`, `KeyDispatcher`, `KeyOutcome`, `route_key`, `action_for` | key press to `KeyChord`, chord to command (default keymap plus `keybindings.toml`), command to `WorkspaceAction`; conflict winner and notices |
| `palette` | `PaletteState`, `PaletteItem` | open/closed, query and selection; lists `CommandRegistry::query` results with each command's shortcut |
| `browser` | `ObjectTree`, `TreeNodeId`, `TreeNodeKind`, `LoadState`, `MenuEntry`, `menu_for`, `BrowserAction`, `DropFlow`, `ObjectTab`, `BrowserError`, jobs | the object browser: lazy tree per connection, menus, commands `object:*`, DROP confirmation, DDL and properties tab content, async jobs |
| `workspace` | `Workspace`, `WorkspaceAction`, `Effect`, `RequestId`, `Purpose`, `SecretOp`, `SessionBridge` | owns sidebar, tabs and sessions; dispatches actions |

`cargo test -p sqler-ui` runs these on default features without GPUI. The views (`sidebar/view.rs`, `tabs/view.rs`, `connections/view.rs`, `workspace/view.rs`) exist only with the `gui` feature, which `sqler-app` enables. They only render state; CI builds them but no test opens a window.

## Action, command, effect

```text
click / key -> WorkspaceAction -> CommandId (T4) -> Workspace::dispatch -> Effect -> view carries it out
```

| Action | Command ID |
| ------ | ---------- |
| `SelectConnection` | `connection:select` |
| `OpenConnection` | `connection:open` |
| `ActivateTab` | `tab:activate` |
| `CloseTab`, `CloseActiveTab` | `tab:close` |
| `NextTab` | `tab:next` |
| `PreviousTab` | `tab:previous` |
| `Quit` | `app:quit` |
| `OpenSettings` | `settings:open` |
| `CloseSettings` | `tab:close` |
| `ToggleThemeMode` | `theme:toggle-mode` |
| `SetTheme` | `settings:set-theme` |
| `SetUiFontSize` | `settings:set-ui-font-size` |

`register_commands` adds these to the `CommandRegistry`; `connections::register_commands` adds the 14 `connection:*` and `connection-group:*` commands (`connection:new`, `edit`, `delete`, `test`, `cancel-test`, `save`, `cancel`, `move-to-group`, `trust-host-key`, `reject-host-key`, `connection-group:new`, `rename`, `delete`, `toggle`). The workspace `action_for` runs first, then `connections::action_for`, which uses the selection and the open dialog. `tab:close`, `tab:next`, `tab:previous` and `app:quit` match the default keymap of `sqler-config`.

`dispatch` (and `dispatch_connection` for `ConnectionAction`) changes state and returns an `Effect`:

| Effect | The view |
| ------ | -------- |
| `None` | nothing |
| `Connect { request, connect }` | spawns the connect task through the bridge and stores it by `RequestId`; dropping the task cancels it |
| `CancelConnect(request)` | drops that task |
| `SaveConnections { secrets }` | calls `save_connections()`, then writes the `SecretOp`s (set or delete keychain entries) with `spawn_blocking`; failures become notices |
| `TrustHostKey { key, retry }` | `Connector::trust_host_key`, then `trust_finished` repeats `retry` |
| `CloseSession(session)` | closes one session on the runtime |
| `CloseSessions(sessions)` | closes all concurrently, 2 s bound, then quits |
| `OpenGrid`, `Quit` | as before |
| `ApplySettings` | saves the settings and re-applies theme and font size |

`Dialog` (form, delete confirmation, group name, move to group, host-key trust) is state in `Workspace`; `connections/view.rs` renders it as an overlay. Escape closes it.

The Settings tab is a single tab owned by `Workspace` (it is not a `TabStrip` entry) and sits after the last connection tab. It is drawn at the right end of the tab strip and replaces the tab body while active.

## Bridge to tokio

The UI thread has no tokio context, so it never calls `tokio::spawn`.

```text
GPUI thread                          tokio runtime (sqler-db)
-----------                          ------------------------
Effect::Connect
  SessionBridge::connect  ---------> Handle::spawn(connector.connect(request))
  PendingConnect (future)  <-------- oneshot::send(Result<Session, ConnectFailure>)
cx.spawn awaits it
  Workspace::connect_finished(request, result)
    Ok, Purpose::Open -> new Query tab + stored session
    Ok, Purpose::Test -> session closed, form shows "Connected"
    UnknownHostKey    -> host-key dialog
    Err               -> sidebar entry Failed(msg) / form shows the error
```

The bridge holds a `dyn Connector` and a `dyn SecretStore`; it never sees a driver or `sqler-tunnel`. `sqler-app` supplies `Pipeline` (secret, tunnel, TLS and target mapping, driver registry, session wrapper that closes the tunnel), see [ADR-0016](../adr/0016-connect-pipeline.md). Dropping a `PendingConnect` aborts the task and closes a session that was already produced. `SessionBridge::close` and `close_all` close sessions on the runtime. A result for a request nobody waits for any more closes its session (`Effect::CloseSession`).

## Object browser

[ADR-0017](../adr/0017-object-browser-sessions-and-drop-confirmation.md). `sqler_ui::browser` is pure (no GPUI) and has all the logic; `workspace/browser.rs` wires it in and the views only draw it.

```text
click / key -> object:* command -> Workspace::dispatch_object -> ObjectTree::expand -> Vec<CacheKey>
  Effect::LoadCatalog -> cx.spawn: Catalog::children / table (the catalog worker, off the UI thread)
  Workspace::catalog_loaded -> ObjectTree::children_loaded / table_loaded -> rows() rebuilt
```

The tree never calls the catalog. `expand` and the `*_loaded` methods return the `CacheKey`s that still need loading, and the workspace turns them into effects. `rows()` is cached in the tree, so a frame does not rebuild it; the sidebar turns connection rows and tree rows into `SidebarItem`s and the view keeps a GPUI `list` / `ListState` in step with them by splicing the changed range, so only visible rows build elements.

Sessions:

| Session | Owner | Used for |
| ------- | ----- | -------- |
| Browser session | one `CatalogWorker` per connected connection (`Purpose::Browse` connect, started after the first successful open) | `Catalog::children`, `table`, refresh with progress and cancel |
| Short-lived session | `Connector::connect` per job, closed when the job ends | open data (`select_star` into a grid tab), `Introspect::definition` for CREATE text, DROP |

Effects added: `StartCatalog`, `LoadCatalog`, `WatchRefresh`, `RunJob` (DDL, properties, DROP), `OpenData`, `Copy`, `ShowObjectMenu` and `Batch`. The only way to a DROP job is `DropFlow::confirm`: once, twice on a `prod` connection. A DROP that has not started is discarded when a connection dialog opens. Closing a DDL, properties or data tab cancels its job. On quit the bridge closes the catalog workers within a bound (`SessionBridge::wait_all`).

`object:show-menu` (Shift+F10, Menu) opens the palette filtered to `Object: `, because `gpui-component` 0.8.0 cannot open a context menu from code. Right-click uses `ContextMenuExt` with the entries of `menu_for`. Up, Down, Left, Right and Enter on the focused sidebar go through `sidebar::key_command` to `object:*` commands. `TabKind::Ddl` and `TabKind::Properties` are read-only text and property-table bodies.

Benchmark: [Object browser tree](../benchmarks.md#object-browser-tree).

## Query execution

[ADR-0024](../adr/0024-query-execution.md). `sqler_ui::query` is pure (no GPUI); `workspace/query.rs` wires it in and `query/view/**` only draws it.

```text
query:run-* -> EditorViews::buffer (EditorBuffer) -> Workspace::run_editor -> QueryRunPort -> Workspace::run_query
  plan -> ParamForm (prompt) -> prod confirmation -> bind -> Effect::RunQuery
  bridge.spawn(QueryJob::run) -> RunEvent stream -> Workspace::query_event
  QueryJobDone -> Workspace::query_finished (the session goes back to the tab)
```

`QueryTab` (one per query tab, in `Workspace::queries`) is the state machine: `Idle`, `Prompting`, `Confirming` and `Running`, plus the result entries (`Pending`, `Running`, `Done`, `Failed`, `Cancelled`, `NotRun`). Stale `RunId`s are ignored. While a run is active the tab session is on loan to the job; the workspace keeps its `CancelHandle`, the job's `RunStop` (a flag plus the sink cancellers, so a stop wakes a paused statement), and `cancel_query` returns `Effect::CancelQuery`. If the tab closes during a run, `query_finished` closes the returned session. Effects added: `RunQuery` and `CancelQuery`. Each statement has its own `GridSink` and `GridTab` (the M1 grid, as for open data). The SQL editor (ADR-0025) supplies the buffer: `editor::QueryRunPort` is the `RunPort` that turns an `EditorBuffer` into a `BufferSnapshot` and calls `run_query` or `cancel_query`, and the editor element is the top of `query::view::query_tab_body`. User-facing description: [Running queries](../user-guide/running-queries.md).
## Result model

[ADR-0027](../adr/0027-bounded-result-model.md). `grid_channel_with(GridLimits)` grants one page of credit; `ingest` sends rows only against credit and, when it runs out, sends `Msg::Paused` and awaits a std `poll_fn` future, so the driver stream is not polled. `GridModel::fetch_more` grants the next page and wakes it. `GridCancel`, `GridModel::cancel` and dropping the model also wake it; a bare `CancelFlag` does not. Rows are stored by `grid/store.rs` as `Chunk`s built on the ingestion thread: 16-byte cells in one row-major vector, with text, JSON and bytes in arenas and rare variants in a side vector. Readers get `RowRef` and `CellRef`. A paused statement holds the tab session, so later statements of a script wait. User-facing description: [Results grid](../user-guide/results-grid.md).

## SQL editor

The `TabKind::Query` tab is the SQL editor. Its text lives in a `gpui-component` `EditorState` (code editor, language `sql`, tree-sitter highlighting that follows `Theme::change`). The logic is headless in `sqler_ui::editor`; only `editor/view.rs` (`EditorViews`, one editor view per tab, like `GridViews`) uses GPUI.

- `EditorBuffer` (text, selection, cursor; byte offsets) is a snapshot the view copies out of the `EditorState` when a command runs. The logic works on it and returns an `Effect` (`EditorEdit`, `EditorSelect`, `EditorAddSelection`, `EditorKey`) that the view applies. A format edit is applied with `EditorState::replace`, one undo step.
- `EditorDocs` (owned by `Workspace`) keeps per-tab headless state: the dialect, a seed text for a new editor (from "Open in SQL Editor" or a DDL tab) and the chain of added occurrences.
- Modules: `action` (command IDs, `action_for`), `format`, `brackets` (`BracketIndex`, built only when the text changed and the cursor touches a bracket), `occurrence`, `run` and `synthetic`. The workspace side is `Workspace::dispatch_editor`, `run_editor` and `cancel_editor_run`; user messages go through `notice`.
- `RunPort` is the seam to query execution: `run(workspace, tab, RunMode, EditorBuffer)` and `cancel`. Tests use a fake. The real adapter is `QueryRunPort` (`editor/run_t8.rs`).
- Benchmark: `cargo bench -p sqler-ui --bench editor_buffer` (10,000-line script; bracket build and format p95 under 16 ms).

Decision and the gaps in `gpui-component` (no add-next-occurrence, no bracket matching): [ADR-0025](../adr/0025-sql-editor-component.md). User view: [SQL editor](../user-guide/sql-editor.md).

### Intellisense

`sqler_ui::editor::intellisense` (GPUI-free) adapts the connection's browser catalog to `sqler-completion` (ADR-0026). Data flow per request:

1. `Workspace::editor_catalog(tab)` gives the browser generation and `Catalog` of the tab's connection, only while the browser is `Ready`.
2. The provider in `editor/view/provider.rs` (the only GPUI part) takes a fresh `CatalogSnapshot` (ADR-0022) and builds a `CatalogSchema`, a `SchemaSource` over it.
3. `complete_popup`, `hover_markdown` and `diagnose` run on GPUI's background executor, so a refresh shows up on the next keystroke.
4. Requests the engine could not answer (`SchemaRequest`) become `CacheKey`s (`keys_for`); `load` sends them to the catalog, and the futures are awaited on the GPUI executor (never `tokio::spawn`; the catalog worker already runs on tokio). The popup is then asked again.
5. `TabIntel` remembers per tab which keys were asked, the browser generation and the snapshot generation, so a failed load is not repeated and a new snapshot re-runs the debounced (300 ms) diagnostics.

Benchmark: `cargo bench -p sqler-ui --bench editor_intellisense` (5,000 tables; worst p95 under 16 ms). User view: [SQL editor](../user-guide/sql-editor.md#intellisense).

## Query history and saved queries

[ADR-0021](../adr/0021-query-history-and-saved-queries.md) is the store; the UI follows the pattern of [ADR-0024](../adr/0024-query-execution.md). `sqler_ui::history` is headless (no GPUI): `model` (`HistoryTab`, `HistoryRow`, `SavedState`, `HistoryDialog`, `SavedPicker`), `record` (`history_entry`), `action` (the nine `history:*` and `query:*` commands) and `service` (`HistoryService`). `workspace/history.rs` is the `Workspace` side and `history/view.rs` only draws it.

```text
Workspace::query_event (terminal RunEvent) -> history_entry -> Effect::RecordHistory
  SessionBridge::blocking -> HistoryService::record -> Workspace::history_recorded -> SearchHistory (open History tabs)
history tab text/scope change -> Effect::SearchHistory -> blocking search -> Workspace::history_results (stale request ignored)
saved query edit (generation += 1) -> Effect::SaveSavedQueries -> blocking save_saved -> Workspace::saved_queries_written
```

- Recording: `query_event` records the start time on `Started`. A `Finished`, `Failed` or `Cancelled` event becomes a `NewHistoryEntry` from `ResultEntry.sql`, the statement text before binding, so bound values never reach the store. Failed entries carry no message.
- Effects added: `RecordHistory`, `SearchHistory`, `ClearHistory` and `SaveSavedQueries`. Their `Debug` output never prints SQL text.
- `HistoryService` is a blocking facade over `HistoryStore` and `queries.toml`. It opens the store lazily on first use and every call runs on a tokio blocking thread through `SessionBridge::blocking`, because the store lock is held during append and compaction. Without a config directory, recording is a no-op and saved queries report an error.
- Saved queries: edits need `SavedLoad::Ready`, so a `queries.toml` that failed to load is never overwritten. Each change bumps a `generation`, and `save_saved` skips a snapshot older than the last one written, so overlapping writes end with the newest state.
- Open and run again reuse `open_editor_on`. Run again refuses an entry for which `HistoryEntry::redacted()` (the redactor's own placeholder, `contains_redaction`) is true; open only adds a notice. `Workspace` keeps a `PendingOpen` per connect request (`Run(sql)` or `Saved(id)`); when `connect_finished` activates the new Query tab, the hook runs the SQL through `run_query` (so the `prod` confirmation applies) or binds the tab to the saved query.
- A History tab is a `TabKind::History` tab bound to a connection, with title `History: <connection>`; one tab per connection is reused. Dialogs (name, delete and clear confirmations) and the saved-query picker are refused while a connection dialog is open.

User view: [Query history and saved queries](../user-guide/query-history.md).

## Hook points

- Themes (T7): the shell paints only with `gpui_component` theme tokens (`cx.theme()`), with no second palette. `theme::apply` (gui only) calls `Theme::change` and sets `Theme::font_size`. `workspace::run` applies the startup settings, `WorkspaceView::new` observes the OS appearance, and `Effect::ApplySettings` saves and re-applies. The prod tint uses the theme's `danger` token.
- Settings tab (T7): `settings::view::{strip_end, page}` are drawn by the `workspace/view.rs` render (strip row and body switch). `tabs::view::tab_strip` takes `highlight_active`, which is false while the Settings tab is active. `Startup.settings` carries the `SettingsModel`.
- Keymap (T6): done. One `capture_key_down` listener on the workspace root converts the key press to a `KeyChord` and calls `keymap::route_key`; the views use no GPUI `actions!`, because bindings are runtime data. `WorkspaceView::run_command` carries out the result. See [Keybindings](../user-guide/keybindings.md).
- Grid (T8): done. The grid is a `TabKind` variant (it is `#[non_exhaustive]`) rendered in the tab body, over the 1M-row model of [ADR-0006](../adr/0006-grid-data-model-and-bench-harness.md). See [Results grid](../user-guide/results-grid.md) and the [benchmarks](../benchmarks.md).

## Startup

`sqler-app` loads `connections.toml` (`load_connection_store`) and the keybinding overrides from `keybindings.toml`, builds the driver registry, the `KeychainStore` and the `Pipeline`, starts the runtime and calls `sqler_ui::workspace::run(Startup)`. `Startup` carries `connections: Result<ConnectionStore, String>`, `config_dir: Option<ConfigDir>` (where to save; only set when the store loaded), `connector`, `secrets`, `runtime`, `exit_after_first_frame`, `settings`, `keybindings` and `history_dir: Option<ConfigDir>` (where `history.jsonl` and `queries.toml` live; set independently of the connections load result). A load error is shown in the sidebar and does not stop the app, and nothing is saved over a file that failed to load. `--exit-after-first-frame` makes `run` exit with code 0 after the first frame.

Open sessions are closed on quit: the `app:quit` command goes through `Effect::CloseSessions`; the `cx.on_app_quit` hook (window close) has only a 150 ms bound because the platform waits about 200 ms.
