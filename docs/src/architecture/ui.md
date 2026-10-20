# UI workspace

The M1 shell lives in `sqler-ui` and the `sqler-app` binary. Decisions: [ADR-0001](../adr/0001-ui-framework-spike-plan.md) (GPUI) and [ADR-0007](../adr/0007-desktop-binary-and-ui-tokio-bridge.md) (binary, `gui` feature, bridge).

## State and views

State is plain Rust and has no GPUI types:

| Module | Types | Role |
| ------ | ----- | ---- |
| `sidebar` | `SidebarModel`, `SidebarEntry`, `ConnectionStatus` | saved connections in file order, selection, `Idle`/`Connecting`/`Failed` status, load error |
| `tabs` | `TabStrip`, `Tab`, `TabId`, `TabKind`, `TabConnection`, `TabHeader`, `Rgb` | tabs bound to one connection each; titles `name`, `name (2)`; header data (title, accent, badge, prod flag) |
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

`register_commands` adds these to the `CommandRegistry`. `tab:close`, `tab:next`, `tab:previous` and `app:quit` match the default keymap of `sqler-config`. `dispatch` changes state and returns an `Effect`: `None`, `Connect { request, config }`, `CloseSession(session)` or `Quit`.

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

- Keymap (T6): map chords to the command IDs above; the views use no GPUI `actions!` yet.
- Themes (T7): `init(cx)` in `workspace/view.rs` calls `gpui_component::init`; apply the theme there. The prod tint uses the theme's `danger` token.
- Grid (T8): add a `TabKind` variant (it is `#[non_exhaustive]`) and render it in the tab body.

## Startup

`sqler-app` loads `connections.toml`, builds the registry, starts the runtime and calls `sqler_ui::workspace::run(Startup)`. A load error is shown in the sidebar and does not stop the app. `--exit-after-first-frame` makes `run` exit with code 0 after the first frame.
