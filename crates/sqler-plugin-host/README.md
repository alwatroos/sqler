# sqler-plugin-host

Plugin loading, sandboxing, capability grants and lifecycle.

Status: engine-free core, wasmtime runtime (`cl`, `pulley-rt`) and `NetEgress` (`net`) are in (P4, ADR-0057). The typed calls for the UI, catalog, query, driver, export, completion and AI interfaces come with P5 and P8 to P12; their imports are gate-checked stubs today.

Layer: L3

## Layers and features

- Default features: no wasmtime. Discovery (`discover`), manifest and API checks, component digest and grant key,
  `fs.read` scope resolution (`resolve_preopens`), the `GrantSource`, `SettingsStore` and `Egress` traits, error types.
  Declarative plugins load through this layer alone.
- `cl`: the wasmtime runtime with Cranelift. `pulley-rt`: the runtime with the Pulley interpreter (links, but returns
  `NoCompiler` until precompiled components are decided). Both enable the internal `runtime` feature.
- `net`: `NetEgress` over the workspace reqwest/rustls (ring) client.

`sqler-app` does not depend on this crate's features yet: `cargo tree -p sqler-app -i wasmtime` stays empty.

## Tests

`cargo test -p sqler-plugin-host --features cl,net` runs everything except the guest tests. Those use the built `smoke`
and `probe` plugins: `cargo run -p xtask -- plugins build`, then
`SQLER_PLUGIN_FIXTURES=target/plugins/debug cargo test -p sqler-plugin-host --features cl,net --test guests`
(`SQLER_TEST_REQUIRE_PLUGINS=1` makes missing fixtures a failure). `cargo run --release --example probe --features cl,net -- <plugin dir>`
prints engine, compile and instantiate timings.

Design: [ADR-0057](../../docs/src/adr/0057-plugin-host-runtime.md), [architecture page](../../docs/src/architecture/plugins.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
