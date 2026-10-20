# UI workspace

The M1 shell lives in `sqler-ui` and the `sqler-app` binary. Decisions: [ADR-0001](../adr/0001-ui-framework-spike-plan.md) (GPUI) and [ADR-0007](../adr/0007-desktop-binary-and-ui-tokio-bridge.md) (binary, `gui` feature, bridge).

## State and views

State is plain Rust and has no GPUI types:

| Module | Types | Role |
| ------ | ----- | ---- |
| `sidebar` | `SidebarModel`, `SidebarEntry`, `ConnectionStatus` | saved connections in file order, selection, `Idle`/`Connecting`/`Failed` status, load error |
| `tabs` | `TabStrip`, `Tab`, `TabId`, `TabKind`, `TabConnection`, `TabHeader`, `Rgb` | tabs bound to one connection each; titles `name`, `name (2)`; header data (title, accent, badge, prod flag) |
| `theme` | `Appearance`, `resolve`, `toggled` | turns `ThemePreference` and the OS appearance into light or dark; `toggled` gives the explicit opposite |
| `settings` | `SettingsModel` | the `Settings` of `sqler-config`: checked changes (theme, font size 8 to 48), saved only through `ConfigDir::save_settings`; defaults and no writes when loading failed |
| `keymap` | `KeyInput`, `KeyDispatcher`, `KeyOutcome`, `route_key`, `action_for` | key press to `KeyChord`, chord to command (default keymap plus `keybindings.toml`), command to `WorkspaceAction`; conflict winner and notices |
| `palette` | `PaletteState`, `PaletteItem` | open/closed, query and selection; lists `CommandRegistry::query` results with each command's shortcut |
| `workspace` | `Workspace`, `WorkspaceAction`, `Effect`, `RequestId`, `SessionBridge` | owns sidebar, tabs and sessions; dispatches actions |

`cargo test -p sqler-ui` runs these on default features without GPUI. The views (`sidebar/view.rs`, `tabs/view.rs`, `workspace/view.rs`) exist only with the `gui` feature, which `sqler-app` enables. They only render state; CI builds them but no test opens a window.

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

`register_commands` adds these to the `CommandRegistry`. `tab:close`, `tab:next`, `tab:previous` and `app:quit` match the default keymap of `sqler-config`. `dispatch` changes state and returns an `Effect`: `None`, `Connect { request, config }`, `CloseSession(session)`, `ApplySettings` or `Quit`. `ApplySettings` makes the view save the settings and re-apply the theme and font size.

The Settings tab is a single tab owned by `Workspace` (it is not a `TabStrip` entry) and sits after the last connection tab. It is drawn at the right end of the tab strip and replaces the tab body while active.

## Bridge to tokio

The UI thread has no tokio context, so it never calls `tokio::spawn`.

```text
GPUI thread                          tokio runtime (sqler-db)
-----------                          ------------------------
Effect::Connect
  SessionBridge::connect  ---------> Handle::spawn(registry.connect(config))
  PendingConnect (future)  <-------- oneshot::send(result)
cx.spawn awaits it
  Workspace::connect_finished(request, result)
    Ok  -> new Query tab + stored session
    Err -> sidebar entry Failed(msg)
```

Dropping a `PendingConnect` aborts the task and closes a session that was already produced. `SessionBridge::close` closes a session on the runtime. A result for an unknown `RequestId` is dropped (the session is not closed).

## Hook points for later tasks

- Themes (T7): the shell paints only with `gpui_component` theme tokens (`cx.theme()`), with no second palette. `theme::apply` (gui only) calls `Theme::change` and sets `Theme::font_size`. `workspace::run` applies the startup settings, `WorkspaceView::new` observes the OS appearance, and `Effect::ApplySettings` saves and re-applies. The prod tint uses the theme's `danger` token.
- Settings tab (T7): `settings::view::{strip_end, page}` are drawn by the `workspace/view.rs` render (strip row and body switch). `tabs::view::tab_strip` takes `highlight_active`, which is false while the Settings tab is active. `Startup.settings` carries the `SettingsModel`.
- Keymap (T6): done. One `capture_key_down` listener on the workspace root converts the key press to a `KeyChord` and calls `keymap::route_key`; the views use no GPUI `actions!`, because bindings are runtime data. `WorkspaceView::run_command` carries out the result. See [Keybindings](../user-guide/keybindings.md).
- Grid (T8): add a `TabKind` variant (it is `#[non_exhaustive]`) and render it in the tab body.

## Startup

`sqler-app` loads `connections.toml` and the keybinding overrides from `keybindings.toml`, builds the registry, starts the runtime and calls `sqler_ui::workspace::run(Startup)`. A load error is shown in the sidebar and does not stop the app. `--exit-after-first-frame` makes `run` exit with code 0 after the first frame.
