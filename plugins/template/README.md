# {{display_name}}

A SQLer plugin (`ui-plugin` world): one command, `Say hello`, and one status item that counts how often it ran. It asks
for no capability.

## Build

```sh
cargo build --release --target wasm32-wasip2
```

The `rust-toolchain.toml` in this folder pins the Rust version and the `wasm32-wasip2` target. The result is
`target/wasm32-wasip2/release/{{crate_name}}.wasm`, the file `sqler.toml` names.

## Install

Copy `sqler.toml` and the `.wasm` file into a folder named after the plugin id, inside the SQLer plugin directory:

```text
<config>/plugins/{{project-name}}/sqler.toml
<config>/plugins/{{project-name}}/{{crate_name}}.wasm
```

SQLer asks for approval the first time it loads the plugin. The SQLer repository has a tutorial and the full API
reference (`docs/src/plugin-development/`).

## Files

- `sqler.toml`: manifest (id, name, version, API version, capabilities, contributions).
- `src/lib.rs`: the guest.
- `wit/`: a copy of the plugin API (`sqler:plugin`). Do not edit it; replace it when you move to a newer SQLer.
