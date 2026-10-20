# 0007 Desktop binary and UI-thread/tokio bridge

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

ADR-0001 accepted GPUI. M1 needs the first real window: a connection sidebar and a tab strip, where every tab is bound to one connection. Three questions follow. Where does the binary live, given that no workspace crate may depend on `sqler-ui`? How do the GPUI types stay out of the headless tests? How does the UI open a driver session, which needs tokio, without blocking the UI thread (AGENTS.md §4)? GPUI has its own executor and there is no tokio context on its thread. The GPUI dependency tree also brings licences and advisories that `deny.toml` does not allow yet.

## Decision drivers

- Only a desktop binary may depend on `sqler-ui`; driver crates are wired only by binaries.
- Workspace, sidebar and tab state must be testable without a window or a display.
- The UI thread never blocks; every spawned task has a cancellation path.
- Later tasks (keymap, themes, grid) hook into the same view files under `crates/sqler-ui/src/**`.
- A new licence or advisory exception needs an ADR (ADR-0005).

## Considered options

1. A binary crate `sqler-app` and GPUI views behind an optional `gui` feature of `sqler-ui`.
2. A separate `sqler-ui-gpui` crate for the views.
3. Views inside `sqler-app`.
4. For the bridge: `async-compat`, or running the database work on the GPUI executor.
5. For the bridge: `Handle::spawn` on a tokio runtime, result back on a `tokio::sync::oneshot`.

## Decision outcome

Chosen: options 1 and 5.

Binary and layer:

- `crates/sqler-app` is a new crate with the binary `sqler`, layer L7 (above `sqler-ui` and `sqler-cli`). It is the only crate that depends on `sqler-ui`. It builds the `DriverRegistry` (feature `driver-sqlite`, on by default), owns a tokio multi-thread runtime (2 worker threads, named `sqler-db`), loads `connections.toml` through `sqler-config` and calls `sqler_ui::workspace::run`.
- The CI "Dependency rules" step in the `deny` job checks that the reverse dependencies of `sqler-ui` are exactly `sqler-app` and `sqler-ui`, and that only one GPUI is in the tree.

The `gui` feature:

- `sqler-ui` has a cargo feature `gui` (not default) with the optional dependencies `gpui-component` 0.8.0, `gpui` (`gpui-pre` =0.3.9) and `gpui_platform` (`gpui-pre-platform` =0.3.9). The state types (`SidebarModel`, `TabStrip`, `Workspace`, `SessionBridge`) are plain Rust, so `cargo test -p sqler-ui` builds no GPUI and opens no window. The views (`*/view.rs`) are compiled only with `gui`, which `sqler-app` turns on. A workspace build compiles GPUI; the views are build-only on CI.
- Views only render state. Every click becomes a `WorkspaceAction`, which maps to a command ID (T4), runs through `Workspace::dispatch` and returns an `Effect` that the view carries out.

The bridge:

- `SessionBridge::connect` calls `Handle::spawn` on the tokio runtime handle. The task runs `registry.connect(&config)` and sends the result through a `tokio::sync::oneshot`. The receiver does not depend on a runtime, so the view awaits it on GPUI's foreground executor (`cx.spawn`). `tokio::spawn` is never called on the GPUI thread, because there is no runtime context there.
- `PendingConnect` is the future the view awaits. Dropping it aborts the task (`AbortHandle`). If the session was already produced and sits in the channel, or the send fails because the receiver is gone, the session is closed and not leaked. `SessionBridge::close` spawns `Session::close` and reports an error on stderr.
- One driver session per tab.

`deny.toml` additions (the GPUI tree), each a per-crate or per-advisory entry, no global change:

| Entry | Kind | Reason |
| ----- | ---- | ------ |
| `cbindgen` | licence exception, `MPL-2.0` | build dependency of `gpui-pre-apple` |
| `dwrote` | licence exception, `MPL-2.0` | GPUI font handling on Windows |
| `option-ext` | licence exception, `MPL-2.0` | via `dirs-sys` |
| RUSTSEC-2024-0384 | advisory ignore | `instant` unmaintained |
| RUSTSEC-2024-0436 | advisory ignore | `paste` unmaintained |
| RUSTSEC-2025-0134 | advisory ignore | `rustls-pemfile` unmaintained (GPUI HTTP client) |
| RUSTSEC-2026-0206 | advisory ignore | `rustybuzz` unmaintained (text shaping) |
| RUSTSEC-2026-0192 | advisory ignore | `ttf-parser` unmaintained |
| RUSTSEC-2026-0371 | advisory ignore | `zbus_names` unmaintained (Linux desktop integration) |
| RUSTSEC-2026-0370 | advisory ignore | `zvariant` unmaintained |
| RUSTSEC-2026-0372 | advisory ignore | `zvariant_derive` unmaintained |

All eight advisories are "unmaintained" notices with no fixed version; none is a vulnerability. MPL-2.0 is file-level weak copyleft and the crates are used unmodified as dependencies. No GPL, LGPL or AGPL crate appeared. Revisit the ignores on every GPUI bump. This supersedes the remark in ADR-0005 that `option-ext` is not allowed: it is now allowed for that one crate only, and `sqler-config` still uses `std` only.

CI (`.github/workflows/ci.yml`):

- Linux, jobs `check` and `docs`: `sudo apt-get install -y --no-install-recommends pkg-config libfontconfig-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev`. This is the build-only set; runtime libraries (Vulkan) are not installed, because no test opens a window.
- macOS, job `check`: the Metal toolchain guard from the spike workflow.

## Consequences

- Good: the state is tested headless and fast; the GPUI build cost is paid only by the app and by the workspace-wide build.
- Good: the UI thread never waits on a driver; closing a tab or a dropped request cannot leak a session.
- Good: later tasks add view files in the same crate.
- Bad: workspace builds on CI now compile GPUI on three OSes, which is slow; rust-cache limits it. The first run shows the real time.
- Bad: three MPL-2.0 exceptions and eight advisory ignores to maintain.
- Bad: on quit the open sessions are dropped with the process and not closed (`application().run` does not return on macOS). M2 must close sessions on quit.
- Open: the 2-minute interactive macOS check left over from ADR-0001 (trackpad scroll, typing, resizing, and now clicking, opening two tabs on one connection, switching, closing, the prod look) has not been done. An agent cannot do it; it is pending for the maintainer.

## Pros and cons of the options

- Separate `sqler-ui-gpui` crate: isolates GPUI more strongly, but later view tasks are planned under `crates/sqler-ui/src/**`. Rejected.
- Views in `sqler-app`: same planning conflict, and the views could not be reused. Rejected.
- `async-compat` or tokio on the GPUI executor: either adds a dependency and hides the runtime, or puts blocking driver work next to rendering. Rejected.
- `Handle::spawn` plus `oneshot`: no new dependency, explicit cancellation. Chosen.
