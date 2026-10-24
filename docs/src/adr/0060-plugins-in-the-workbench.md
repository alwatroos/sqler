# ADR-0060: Plugins in the workbench

- Status: accepted
- Superseded in part by [ADR-0069](0069-binary-size-budget-70-mb.md) (2026-10-23): the size budget is now 80 MB (83,886,080 bytes, amendment of ADR-0069, decision of 2026-10-23; it was 70 MB, 73,400,320 bytes, before), so the limit of the rule below is now 82,837,504 bytes (83,886,080 minus 1 MiB). The `plugins` feature stays off by default regardless.
- Date: 2026-10-23
- Deciders: SQLer maintainers

## Context and Problem Statement

The plugin API ([ADR-0055](0055-plugin-abi-wit.md), [ADR-0056](0056-plugin-sandbox-and-capabilities.md)), the host
([ADR-0057](0057-plugin-host-runtime.md), [ADR-0058](0058-plugin-query-and-catalog.md)) and the grant store and dialog
([ADR-0059](0059-plugin-capability-grants.md)) exist as libraries. The desktop app still shows nothing of them. Open
questions: where plugins appear in the workbench, how `sqler-ui` talks to a host it must not link (wasmtime), whether the
shipped binary includes the host, how a keyboard-only user approves a plugin, and how the app keeps plugin work off the
first frame and off the database runtime.

## Decision Drivers

- `sqler-ui` stays free of wasmtime (`cargo tree -p sqler-ui -i wasmtime` is empty); the `Connector` (ADR-0016) and
  `AiProviderFactory` (ADR-0050) pattern applies.
- The 60 MB budget of the shipped binary ([ADR-0054](0054-plugin-runtime-and-driver-feasibility.md)); the Linux binary was
  59.6 MiB of 60 MiB before this item.
- No plugin is compiled or instantiated before the first frame (ADR-0007).
- Default deny, and Approve must not be reachable by an accidental key press, yet a keyboard-only and a screen-reader user
  must be able to approve (ADR-0046, ADR-0059 limitation).
- A revoke made in another SQLer window must apply without a restart.
- Plugin writes confirm like the app's own (ADR-0024, ADR-0058).

## Considered Options

- A `PluginsBackend` trait plus plain data in `sqler-ui`, implemented in `sqler-app` over `sqler-plugin-host` behind a
  `plugins` cargo feature. Chosen.
- `sqler-ui` depends on the host crate. Rejected: wasmtime in the UI tree and in every default build.
- The host on by default. Decided by the measured size (below).
- `pulley-rt` instead of `cl`. Rejected (below).

## Decision Outcome

### Surfaces

| Surface | Id / name | Notes |
| ------- | --------- | ----- |
| Activity item | `ActivityItem::Plugins`, title "Plugins", icon `AppIcon::Plug` | Bottom group, before AI and Settings. Always shown. An attention dot when a row needs a decision or is `Faulted` or `LoadFailed`. |
| Side view | `SideView::Plugins`, persisted as `plugins` | A 4th value of `side_view` in `workbench.toml`; additive, no format version bump (ADR-0045). An older build reads it leniently as the default. |
| Commands | `workbench:show-plugins` ("View: Show Plugins"), `plugins:refresh` ("Plugins: Rescan"), `plugins:enable`, `plugins:disable`, `plugins:review-permissions`, `plugins:revoke` | The last four act on the selected row. |
| Dialog | the ADR-0059 permission dialog, opened by the manager | One at a time, a queue for several. |
| Dialog command | `plugins:deny-grant` ("Deny Plugin Permissions") | Registered. |

`plugins:approve-grant` stays an id in `grant::COMMANDS` (the dialog button maps to it) but is **not registered**: the
palette never lists it, `KeyDispatcher` rejects an override for it as unknown and `run_command` never maps it. Approving
is only the Approve button or the typed id below.

For P8 (contributions): a plugin command appears in the command palette as `plugin-<plugin id>:<contribution id>`; a
plugin panel is a view of the bottom panel; a plugin status item is placed right of the built-in status segments; a
context-menu entry goes through the palette-filtered object menu. P7 adds none of them.

### The UI-facing trait

`sqler_ui::plugins::PluginsBackend` (every method blocks: call it on the background executor, never on the UI thread):
`refresh()` (reload grants, discover, register, apply the stored preferences), `snapshot()` (reload grants, refresh the
host's view of the plugins whose grants changed, return the rows), `set_enabled`, `grants_changed`, `set_setting` and
`start` (a future). The data is `PluginSnapshot { rows, problems }`, `PluginRow` (manifest, digest, dev flag, `RowState`,
enabled flag, effective capabilities, last error, scope issues, settings with values) and `PluginProblem`.
`Startup.plugins` is a `PluginsStartup { backend: Option<Arc<dyn PluginsBackend>>, grants: Arc<GrantBook>, writes: WriteInbox }`, built only
through `new` or `unavailable()` (never a literal) so fields can be added. Without the feature the backend is `None` and
the view says "This build has no plugin support".

### Typed-id keyboard path

The dialog has a confirmation field labelled "Type the plugin id {id} to approve". Focus starts on the dialog; the first
Tab stop is **Deny**, the second the field. Enter in the field approves only when the text equals the plugin id exactly
(`GrantPrompt::confirms`, which also requires that the prompt has no problem). The Approve button is still pointer-only,
not a tab stop and ignores keyboard clicks. Escape (`GrantPrompt::key_command`) denies from anywhere in the dialog,
including the field. Typing an id is a deliberate act that a stray Enter or Space cannot produce; the screen reader
announces the id to type.

### Grants are re-read

`GrantBook::reload` re-reads `plugin-grants.toml` (a damaged or newer file locks the book; a valid one unlocks it). The
manager reloads on load, when a dialog opens, on every Plugins command and on window activation. The app's
`GrantSource` wrapper reloads before each answer to the host, so the next call of a plugin sees another instance's
revoke. `snapshot` compares the effective capabilities with the last ones and calls `PluginHost::refresh_grants` for a
plugin that changed, which cancels its calls in flight. A decision answered `StaleDecision` rebuilds the prompt from the
book and shows the dialog again.

### Preferences

`<config>/plugin-prefs.toml` (version 1, mode 0600): `disabled = [ids]` and `[settings.<id>]` values. It is not security
relevant but follows the ADR-0059 file rules (never overwritten when damaged, newer, too large or other-writable). The host
reads settings through `SettingsStore` over it; a plugin never writes.

### Startup, runtime and paths

- `sqler_app::plugins::startup(config, dev, connector)` builds the grant book, the channel of write approvals (its inbox is
  `PluginsStartup.writes`) and an idle backend that lends sessions opened through `connector`. It does no I/O beyond reading
  `plugin-grants.toml`. The first `refresh` is requested from `window.on_next_frame`, so no folder is scanned and no
  plugin is compiled or instantiated on the first-frame path. The host is created on that first refresh;
  `PluginHost::new` itself does nothing.
- Plugins get **their own** tokio runtime (2 workers, thread name `sqler-plugins`), created lazily with the host, not the
  `sqler-db` runtime: a guest spinning on a worker holds it for one poll per epoch tick until its deadline (Command 30 s,
  driver call up to 300 s) and must not starve database work. `Drop` calls `PluginHost::shutdown` and
  `Runtime::shutdown_background`.
- Development paths come only from the repeatable `--plugin-dir <PATH>` flag. `target/plugins` (where the probe
  component is staged) is **never** a plugin path, in any build profile. A build without the feature refuses the flag.
  A development plugin with the id of an installed one shadows it and, once approved, can read `plugins/<id>/data` of
  the other; accepted (ADR-0057) because dev paths are explicit user input.
- `config_plugins` is `<config>/plugins` made absolute and canonical (the longest existing ancestor is canonicalised, the
  rest appended; the scope rules refuse anything else), and the data root's last segment is exactly `data`.
- For a plugin that asks for an `fs.read` scope under `data`, the app creates `<config>/plugins/<id>/data` as a **real
  directory** (parents too, `create_dir`) and checks it with `symlink_metadata`. A pre-placed link or file is refused,
  nothing is followed or removed, and the plugin is not started (last error: the data folder). Plugins without such a scope
  get no folder.

### The `plugins` feature, and the engine

`sqler-app` feature `plugins = ["dep:sqler-plugin-api", "dep:sqler-plugin-host", "dep:sqler-catalog", "sqler-plugin-host/cl", "sqler-plugin-host/net"]`, **off by
default**. The engine is `cl` (Cranelift), which loads a plain `.wasm`. `pulley-rt` is not used: the app would carry no
compiler, so it would need an unbuilt `.cwasm` precompile pipeline and an `unsafe` deserialize (the workspace forbids
`unsafe`), and its +3.6 MB does not fit the headroom either.

The rule, from ADR-0054: the feature is on by default only if the release binary **with** it stays below
62,914,560 - 1,048,576 = 61,865,984 bytes on **every** OS (60 MB minus a 1 MiB reserve). `size.yml` builds both flavours
and reports both and the delta per OS. Measured figures:

| Build | macOS arm64, local (release, `--locked`) | Linux | Windows |
| ----- | ---------------------------------------- | ----- | ------- |
| default | 38,186,592 bytes (36.4 MiB) | 62,512,360 bytes (59.6 MiB) at the M5 close, `size.yml` run 39780918427; 402,200 bytes under the budget | `size.yml` |
| `--features plugins` | 49,272,272 bytes (47.0 MiB) | `size.yml` of the PR (expected about +14 MB, ADR-0054 measured +14.5 MB for `cl`) | `size.yml` of the PR |
| delta | +11,085,680 bytes (+10.6 MiB) | CI | CI |

The macOS figures were measured on the development machine (macOS arm64, reference machine class) on the uncommitted tree of this item; the
Linux and Windows figures come from the `Binary size` workflow of the pull request, which builds both flavours and
reports both (see the PR). **Decision: off by default.** The Linux binary without the host is already 402,200 bytes under
the budget, less than the 1 MiB reserve, so the rule fails on Linux for any delta, and `cl` alone adds about 10 to 14 MB.
macOS would fit (49.3 MB of 62.9 MB); the rule needs every OS. The reason is recorded here, the figures of the CI run are
in the PR, and the question is reopened when the Linux headroom grows (a smaller default build) or a second, larger artifact
is published (ADR-0054, `plugins-full`).

Cold start, `--exit-after-first-frame` (hyperfine `-N`, 3 warm-ups, 20 runs, scratch `HOME` with a staged plugin; macOS
arm64 local, three rounds in rotating order, all within noise): default 115.8 to 122.3 ms, `plugins` build 117.5 to
123.0 ms, `plugins` build with `--plugin-dir` 117.3 to 123.3 ms (sigma 2 to 9 ms). The feature costs nothing before the
first frame (AC4); the detail is in [benchmarks](../benchmarks.md).

### Plugin writes (AC6)

A plugin write reuses the AI approval shape ([ADR-0053](0053-ai-session-tab-and-approvals.md)): a queue of asks, a card with
the exact SQL, a dropped reply is a denial, and on a `prod` connection a second confirmation through the ADR-0024 gate.
Its text is in the user guide; the app implements the host's `QueryApprover` by sending asks to the UI
(`PluginWriteAsk` on a channel whose receiver `PluginsStartup::writes` hands to the workspace). The host asks `Confirm`
and, on `prod`, `ProdSecond`; the window draws the first as a dialog with Deny focused and the second as the query
editor's production confirmation, opened with `open_prod_gate` on a `QueryTab` the queue owns. Both dialogs name the
plugin by display name and id, and both Approve buttons are pointer-only (no tab stop, keyboard clicks ignored, as in the
grant dialog) and dead for 500 ms after the dialog appears, so a click meant for the grid cannot land on it. The app also implements
`DataService` over the saved connections: every `acquire` opens a fresh session through the connect pipeline (never a
tab's, so no user transaction is lent) and `release` always closes it, so a session whose state a plugin statement
changed (`BEGIN`, `SET ROLE`, `USE`, `ATTACH`, temporary tables) never reaches a pool. `catalog` returns `None` for now.

### `db.read` is flagged

`db.read` is flagged in the permission dialog like every other permission; only `ui.panel` is calm (decided by the dev
team, replacing the calm treatment of ADR-0059 as first written). The Plugins view marks a granted `db.read` and
`net.http` in the capability chips. The user guide says in "Data leaving your machine" that the two together let a
plugin send your data to that host.

## Consequences

- Good: `sqler-ui` stays wasmtime-free; the default binary is unchanged; a keyboard-only user can approve; a revoke in
  another window takes effect for the next call; plugin work cannot slow the first frame or the database runtime.
- Bad: the default download has no plugins until the size budget allows it or a second artifact is published; the typed
  id is new UX; every call re-reads a small file (at most 1 MiB) off the UI thread.
- Follow-up: P8 adds contributions to the surfaces above; P9 to P12 add services to `Services`.

## Addendum (P8): plugin contributions

P8 adds no WIT and no package version bump. Contributions come from the manifest (`Manifest::contributes()`); the
window turns them into registrations and the host answers the plugin's `ui` import.

### Command ids

Every id passes `CommandId::new` and is rebindable in `keybindings.toml`. The form is `plugin-<plugin>:<command>`
(one `:` only, which `CommandId` allows), not `plugin:<id>:<action>`. The categories are `plugin-` (palette,
`<plugin name> (plugin): <title>` so a title never equals a built-in one, called with no context), `object-plugin-`, `editor-plugin-` and `results-plugin-` (menu
entries, each called with its location's context), `panel-plugin-` (show a panel) and the built-in
`workbench:show-plugin-panels`. The prefixes differ, so categories cannot collide with each other or with a built-in id.
The UI builds the registrations from the `PluginRow`s the manager loads, validates and namespaces every id, refuses
invalid ones and duplicates with a one-time notice, and re-syncs the palette, the keymap (the dispatcher and its hints
are rebuilt), the status bar and the bottom panel after every manager load. A plugin that is off, faulted, failed or
not approved loses its contributions at that point. Panels also need an effective `ui.panel`; commands, status items
and menu entries need no capability. `recent_commands` restored at startup drop plugin ids, because those are not
registered yet.

### Placement

Palette commands appear in the command palette. Object entries are in the object menu (Shift+F10 or the Menu key,
`object:show-menu`), editor entries and results entries only in the palette; pointer context menus of the grid and the
editor are not extended. Status items fill in once the row turns `Live` (through `ui-handler.status`) and then from
`set-status` events. A `left` item goes at the end of the left group of the status bar and a `right` item between the
timing and the panel toggles; they are not focusable. Each carries a plug mark, and its tooltip is always the plugin name, preceded by the manifest tooltip when there is one (`<tooltip> - <plugin name>`). Neither is the plugin's to change, so a status text cannot pass as one of SQLer's own items.

### Panel host

`PanelView::PluginPanels` (`"plugin-panels"`, the sixth `BOTTOM_VIEWS` entry, additive per ADR-0045) is the bottom tab
"Plugins". It shows only when at least one panel is registered; the selected panel is not persisted. A headless
`PanelSlot` (`Loading | Tree | Failed`) renders a tree only while the panel is shown, keeps one render or event call in
flight, queues one follow-up refresh and drops events beyond a queue of 8. After a failed call it asks the manager to
reload so a fault shows up. The window calls the plugin through `PluginsBackend` futures run with `SessionBridge::spawn`;
the host sends `refresh-panel`, `set-status` and `notify` over an unbounded channel whose inbox `PluginsStartup` hands
to the workspace, as for writes. The host's typed calls replace the removed `ui_seam` (ADR-0057). A bad tree is a value
(`WidgetError`), not a fault, and shows the error state with a Reload action.

### Caps

- Host, per plugin, token bucket: `refresh-panel` 20 burst and 10 per second, `set-status` 20 and 10 per second,
  `notify` 3 and 1 per 10 seconds. Over-rate calls are dropped with one `tracing::debug!`. A missing sink, an undeclared
  item or `refresh-panel` without `ui.panel` also drops the call.
- Text from a plugin is one line without control characters and cut at a character boundary: status 256 bytes, notify
  512. Notifications are prefixed with the plugin name, which guards against spoofing.
- Window: a status shows at most 64 characters with "…" and at most 8 plugin items; an empty text hides the item. A
  panel renders at most 10 times a second (100 ms apart); an editor selection passed to a command is capped at 256 KiB.
  Tree size uses the existing widget limits. Drawing is capped on top of that: at most 5,000 table cells (headings
  included) per tree are drawn, in tree order; a table that does not fit in what is left shows "N more rows not shown"
  (or a short "not shown" line when not even its headings fit), and each table has its own scroll state. The window
  compares trees by identity and diffs a new one once, so a frame that changes nothing copies nothing.
- Text pulled with `ui-handler.status` gets the same one-line cleaning as pushed text (control characters, bidi
  overrides and isolates, U+2028/2029 removed, 256 bytes), and so does plugin error text shown in a notification. A
  panel always shows a one-line header with the plugin name, and the palette title carries "(plugin)".
- An editor or results command sends the selected SQL (up to 256 KiB) to the plugin, with no permission: the user
  starts it by choosing the command. A panel event runs with the command deadline of 30 s, so a hanging handler blocks
  that panel's queue for that long, not the window.

### Not done

No tracing subscriber is installed (the `debug!` lines go nowhere): it would be a new dependency in a default build;
the size budget is 70 MB now, so room is not the reason; it is left out of this change to keep the default build's dependencies as they are. This is a follow-up.
