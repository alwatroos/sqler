# SQLer plugin guests

This directory is a separate cargo workspace (own `Cargo.toml`, `Cargo.lock`, lints and release profile) for plugin
components built for `wasm32-wasip2`. The root workspace lists it in `exclude`.

Why it is not part of the root workspace: the root `forbid(unsafe_code)` cannot be relaxed for the `wit-bindgen`
glue, the guests need a different release profile (`opt-level = "s"`, `lto`), and the `wit-bindgen` proc-macro tree
would join every host build and the root `Cargo.lock`. Built `.wasm` files are never committed.

## Layout

- `Cargo.toml`: guest workspace (`members`, package defaults, `wit-bindgen`, lints, profile).
- `fixtures/smoke`: the smoke fixture (`sqler-fixture-smoke`, plugin id `smoke`): the `base` world, `init` logs one line.
- `fixtures/probe`: the probe fixture (`sqler-fixture-probe`, plugin id `probe`, world `ui-plugin`): commands that panic, trap, run out of memory, spin, sleep and flood the log, and status items that report what the sandbox exposes (environment, scoped and escaping file reads, HTTP). Driven by `crates/sqler-plugin-host/tests/guests.rs`; `wasip2` provides the wasi:http client types.
- `fixtures/driver`: the driver fixture (`sqler-fixture-driver`, plugin id `driver-fixture`, driver id `fixture-db`, world `driver-catalog-plugin`): a guest driver over a fake in-memory database whose statement text picks the behaviour (streams, affected rows, a spinning batch, a trap in the second batch, malformed batches, catalog). Driven by `crates/sqler-plugin-host/tests/guests/driver.rs`.
- `sqlserver`: the SQL Server plugin (plugin id and driver id `sqlserver`): a declarative host-driver shell with only a `sqler.toml` (no `[entry]`, no guest crate, not a workspace member). The native engine is `crates/sqler-driver-mssql`, linked behind the `driver-mssql` feature. xtask only stages the manifest.
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
