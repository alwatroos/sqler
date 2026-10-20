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

## Hook points

- Themes (T7): the shell paints only with `gpui_component` theme tokens (`cx.theme()`), with no second palette. `theme::apply` (gui only) calls `Theme::change` and sets `Theme::font_size`. `workspace::run` applies the startup settings, `WorkspaceView::new` observes the OS appearance, and `Effect::ApplySettings` saves and re-applies. The prod tint uses the theme's `danger` token.
- Settings tab (T7): `settings::view::{strip_end, page}` are drawn by the `workspace/view.rs` render (strip row and body switch). `tabs::view::tab_strip` takes `highlight_active`, which is false while the Settings tab is active. `Startup.settings` carries the `SettingsModel`.
- Keymap (T6): done. One `capture_key_down` listener on the workspace root converts the key press to a `KeyChord` and calls `keymap::route_key`; the views use no GPUI `actions!`, because bindings are runtime data. `WorkspaceView::run_command` carries out the result. See [Keybindings](../user-guide/keybindings.md).
- Grid (T8): done. The grid is a `TabKind` variant (it is `#[non_exhaustive]`) rendered in the tab body, over the 1M-row model of [ADR-0006](../adr/0006-grid-data-model-and-bench-harness.md). See [Results grid](../user-guide/results-grid.md) and the [benchmarks](../benchmarks.md).

## Startup

`sqler-app` loads `connections.toml` (`load_connection_store`) and the keybinding overrides from `keybindings.toml`, builds the driver registry, the `KeychainStore` and the `Pipeline`, starts the runtime and calls `sqler_ui::workspace::run(Startup)`. `Startup` carries `connections: Result<ConnectionStore, String>`, `config_dir: Option<ConfigDir>` (where to save; only set when the store loaded), `connector`, `secrets`, `runtime`, `exit_after_first_frame`, `settings` and `keybindings`. A load error is shown in the sidebar and does not stop the app, and nothing is saved over a file that failed to load. `--exit-after-first-frame` makes `run` exit with code 0 after the first frame.

Open sessions are closed on quit: the `app:quit` command goes through `Effect::CloseSessions`; the `cx.on_app_quit` hook (window close) has only a 150 ms bound because the platform waits about 200 ms.
