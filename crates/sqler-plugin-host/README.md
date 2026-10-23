# sqler-plugin-host

Plugin loading, sandboxing, capability grants and lifecycle.

Status: engine-free core, wasmtime runtime (`cl`, `pulley-rt`) and `NetEgress` (`net`) are in (P4, ADR-0057). The guarded `catalog` and `query` interfaces are in (P5, ADR-0058). The `ui` import and the typed `ui-handler` calls (`run_command`, `render_panel`, `panel_event`, `status`) are in (P8, ADR-0060): the app inserts one `PluginUi` over its `UiSink` into `Services`, and the import is gated by the manifest's declarations, by `ui.panel` for panels, and by per-plugin rate limits. The typed calls for the driver, export, completion and AI interfaces come with P9 to P12.

Layer: L3

## Layers and features

- Default features: no wasmtime. Discovery (`discover`), manifest and API checks, component digest and grant key,
  `fs.read` scope resolution (`resolve_preopens`), the `GrantSource`, `SettingsStore` and `Egress` traits, error types.
  Declarative plugins load through this layer alone.
- `cl`: the wasmtime runtime with Cranelift. `pulley-rt`: the runtime with the Pulley interpreter (links, but returns
  `NoCompiler` until precompiled components are decided). Both enable the internal `runtime` feature.
- `net`: `NetEgress` over the workspace reqwest/rustls (ring) client.

`sqler-app` does not depend on this crate's features yet: `cargo tree -p sqler-app -i wasmtime` stays empty.

## Data access (runtime layers)

The host never depends on the app. The app puts one `PluginData` value into `Services`: a `DataService` (connections,
catalog, lending a session) and optionally a `QueryApprover`. `query.run` classifies the statement, runs a plain read
with `db.read`, and runs anything else only with `db.write` and an explicit approval of the exact SQL (twice on `prod`).
Caps: 1 MiB SQL, 1000 params, rows and time from the ADR-0056 limits, 8 MiB result. Design and threat model:
[ADR-0058](../../docs/src/adr/0058-plugin-query-and-catalog.md).

## Tests

`cargo test -p sqler-plugin-host --features cl,net` runs everything except the guest tests. Those use the built `smoke`
and `probe` plugins: `cargo run -p xtask -- plugins build`, then
`SQLER_PLUGIN_FIXTURES=target/plugins/debug cargo test -p sqler-plugin-host --features cl,net --test guests`
(`SQLER_TEST_REQUIRE_PLUGINS=1` makes missing fixtures a failure). `cargo run --release --example probe --features cl,net -- <plugin dir>`
prints engine, compile and instantiate timings.

Design: [ADR-0057](../../docs/src/adr/0057-plugin-host-runtime.md), [ADR-0058](../../docs/src/adr/0058-plugin-query-and-catalog.md), [architecture page](../../docs/src/architecture/plugins.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
