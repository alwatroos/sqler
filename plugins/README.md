# SQLer plugin guests

This directory is a separate cargo workspace (own `Cargo.toml`, `Cargo.lock`, lints and release profile) for plugin
components built for `wasm32-wasip2`. The root workspace lists it in `exclude`.

Why it is not part of the root workspace: the root `forbid(unsafe_code)` cannot be relaxed for the `wit-bindgen`
glue, the guests need a different release profile (`opt-level = "s"`, `lto`), and the `wit-bindgen` proc-macro tree
would join every host build and the root `Cargo.lock`. Built `.wasm` files are never committed.

## Layout

- `Cargo.toml`: guest workspace (`members`, package defaults, `wit-bindgen`, lints, profile).
- Examples (small, copyable, no test-only behaviour; each has a README):
  - `hello-panel`: `sqler-example-hello-panel`, plugin id `hello-panel`, world `ui-plugin`: a panel with a click counter, a command and a status item (capability `ui.panel`).
  - `exporter-markdown`: `sqler-example-exporter-markdown`, plugin id `exporter-markdown`, exporter id `markdown`, world `exporter-plugin`: a Markdown table with right-aligned number columns (no capability). Driven, like `hello-panel`, by `crates/sqler-plugin-host/tests/guests/`.
- Fixtures, for the host tests only (`fixtures/*`):
- `fixtures/smoke`: the smoke fixture (`sqler-fixture-smoke`, plugin id `smoke`): the `base` world, `init` logs one line.
- `fixtures/probe`: the probe fixture (`sqler-fixture-probe`, plugin id `probe`, world `ui-plugin`): commands that panic, trap, run out of memory, spin, sleep and flood the log, and status items that report what the sandbox exposes (environment, scoped and escaping file reads, HTTP). Driven by `crates/sqler-plugin-host/tests/guests.rs`; `wasip2` provides the wasi:http client types.
- `fixtures/driver`: the driver fixture (`sqler-fixture-driver`, plugin id `driver-fixture`, driver id `fixture-db`, world `driver-catalog-plugin`): a guest driver over a fake in-memory database whose statement text picks the behaviour (streams, affected rows, a spinning batch, a trap in the second batch, malformed batches, catalog). Driven by `crates/sqler-plugin-host/tests/guests/driver.rs`.
- `fixtures/exporter`: the exporter fixture (`sqler-fixture-exporter`, plugin id `exporter-fixture`, exporter id `tsv`, world `exporter-plugin`): writes tab-separated rows; a text cell `!trap` makes it trap and `!fail` makes it return a guest error. Driven by `crates/sqler-plugin-host/tests/guests/exporter.rs` and the `plugin_export` bench.
- Each plugin directory holds a `sqler.toml` manifest and, for a code plugin, a `cdylib` crate.

## Toolchain

`rust-toolchain.toml` at the repository root pins Rust 1.99.1 and the `wasm32-wasip2` target (ADR-0054), so a bare
`rustup toolchain install` installs both. An older local toolchain may lack the target: `rustup target add wasm32-wasip2`.

## Commands

Run from anywhere in the repository:

```
cargo run -p xtask -- plugins build [--release] [<name>...]   # no names = every plugin
cargo run -p xtask -- plugins check                           # fmt --check + clippy -D warnings (wasm target)
cargo run -p xtask -- plugins list
cargo run -p xtask -- plugins clean
cargo deny --manifest-path plugins/Cargo.toml --config deny.toml check
```

Exit codes: 0 ok, 1 failure, 2 usage error. Root `cargo fmt/clippy --workspace` skips this workspace; use `plugins check`.

## Output

Cargo builds into `target/plugins/cargo`. Each plugin is staged as an installable directory:

```
target/plugins/<debug|release>/<id>/sqler.toml
target/plugins/<debug|release>/<id>/<entry.component>
```

## Adding a plugin

1. Create the crate under `plugins/` with a `sqler.toml` and `[lints] workspace = true`.
2. Add its path to `members` in `plugins/Cargo.toml`.
3. Add a row to the plugin table in `xtask/src/plugins/table.rs`.

## Cleanup

`cargo run -p xtask -- plugins clean` removes `target/plugins`; `cargo clean` removes everything.
