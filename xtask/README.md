# xtask

Repository automation tasks (generate WIT bindings, release, bench, plugin build).

Status: `plugins` task (M6); the other tasks are not written yet

Layer: tooling (outside the layering)

Commands (exit codes: 0 ok, 1 failure, 2 usage error):

```
cargo run -p xtask -- plugins build [--release] [<name>...]
cargo run -p xtask -- plugins check
cargo run -p xtask -- plugins list
cargo run -p xtask -- plugins clean
```

`plugins build` compiles the guest workspace in `plugins/` for `wasm32-wasip2` and stages every plugin as
`target/plugins/<debug|release>/<id>/` (`sqler.toml` plus the component). The plugin table is
`src/plugins/table.rs`.

See [AGENTS.md](../AGENTS.md) for the project handbook.
