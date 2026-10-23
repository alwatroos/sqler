# hello-panel

An example SQLer plugin (world `ui-plugin`) that shows the three UI contributions in one small crate:

- a panel `hello` with a heading, a line that counts clicks and a "Click me" button;
- a command `say-hello` that shows the notification "Hello!" and asks the app to redraw the panel;
- a status item `clicks` that shows `Clicks: N` in the status bar.

The click counter lives in the guest (a static), so it restarts when the plugin is reloaded.

## Capability

The manifest declares `ui.panel`, and nothing else. Without it the host refuses `render-panel` and `panel-event`
(error code `capability-denied`) and drops `refresh-panel` calls. Commands, notifications and status items need no
capability. A capability only counts when the user approved it and the manifest declares it.

## Build and install

```
cargo run -p xtask -- plugins build hello-panel
```

The installable directory is `target/plugins/debug/hello-panel/` (`target/plugins/release/hello-panel/` with
`--release`): `sqler.toml` and `hello-panel.wasm`. In SQLer open the plugin manager and add that directory as a
development plugin, then approve `ui.panel`.

## Ids

The app derives the ids of the contributions from the plugin id and the ids in the manifest:

| Contribution | Id |
| ------------ | -- |
| command | `plugin-hello-panel:say-hello` |
| panel | `panel-plugin-hello-panel:hello` |
| status item | `clicks` of plugin `hello-panel` |

Open the panel with the command `panel-plugin-hello-panel:hello`. A command of the plugin cannot open a panel yet:
the plugin API has no call to show one, so `say-hello` only refreshes the panel when it is already open.

## Tests

The behaviour is tested from the host in `crates/sqler-plugin-host/tests/guests/hello_panel.rs`: granted, ungranted
and undeclared capability.
