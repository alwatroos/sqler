# SQLer plugin guests

This directory is a separate cargo workspace (own `Cargo.toml`, `Cargo.lock`, lints and release profile) for plugin
components built for `wasm32-wasip2`. The root workspace lists it in `exclude`.

Why it is not part of the root workspace: the root `forbid(unsafe_code)` cannot be relaxed for the `wit-bindgen`
glue, the guests need a different release profile (`opt-level = "s"`, `lto`), and the `wit-bindgen` proc-macro tree
would join every host build and the root `Cargo.lock`. Built `.wasm` files are never committed.

## Layout

- `Cargo.toml`: guest workspace (`members`, package defaults, `wit-bindgen`, lints, profile).
- `fixtures/smoke`: the smoke fixture (`sqler-fixture-smoke`, plugin id `smoke`): the `base` world, `init` logs one line.
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
