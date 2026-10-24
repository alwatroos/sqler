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
cargo run -p xtask -- release manifest --plan <dist-manifest.json> --artifacts <dir> --published <YYYY-MM-DDTHH:MM:SSZ> --out <file> [--tag <vX.Y.Z>] [--key-file <file> [--test-key]] [--partial]
cargo run -p xtask -- release verify <envelope-file> [--public-key-file <file>]
```

`plugins build` compiles the guest workspace in `plugins/` for `wasm32-wasip2` and stages every plugin as
`target/plugins/<debug|release>/<id>/` (`sqler.toml` plus the component). The plugin table is
`src/plugins/table.rs`.

`release manifest` reads the cargo-dist plan manifest and the final release archives, builds the update manifest of
`sqler-update` (sizes and SHA-256 per platform), signs it with Ed25519 and checks the result with the client's own
verifier before it writes `--out`. The key is read from a file that others cannot read (never from argv or env), and
it must be one of the keys compiled into `sqler-update`; `--test-key` signs with a throwaway key that is not
compiled in (CI dry run, output is TEST ONLY). Without `--key-file` it writes the bare manifest JSON. `release verify`
checks a signed file against the compiled-in keys, or against `--public-key-file`, and prints the platform entries.
Details: `docs/src/releasing/update-feed.md`.

See [AGENTS.md](../AGENTS.md) for the project handbook.
