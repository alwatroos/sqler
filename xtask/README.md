# xtask

Repository automation tasks (generate WIT bindings, release, bench, plugin build).

Status: `plugins` (M6) and `bench` (R12) tasks; the other tasks are not written yet

Layer: tooling (outside the layering)

Commands (exit codes: 0 ok, 1 failure, 2 usage error):

```
cargo run -p xtask -- plugins build [--release] [<name>...]
cargo run -p xtask -- plugins check
cargo run -p xtask -- plugins list
cargo run -p xtask -- plugins clean
cargo run -p xtask -- bench [--bin PATH] [--archive PATH] [--commit SHA] [--out DIR]
    [--source desktop|ci-runner] [--machine TEXT] [--runs N] [--warmup N] [--rss-runs N]
    [--skip cold-start|rss|size|benches]...
cargo run -p xtask -- release manifest --plan <dist-manifest.json> --artifacts <dir> --published <YYYY-MM-DDTHH:MM:SSZ> --out <file> [--tag <vX.Y.Z>] [--key-file <file> [--test-key]] [--partial]
cargo run -p xtask -- release verify <envelope-file> [--public-key-file <file>]
```

`bench` measures the headline numbers of one release binary: size, cold start (hyperfine over
`--exit-after-first-frame`), idle RSS (`--open-sessions`, three SQLite sessions) and the five `cargo bench` targets.
Cold start and RSS run twice in throwaway config directories under `target/xtask-bench/`: variant `none` with no
plugin, variant `plugin` with the SQL Server manifest installed and approved. Without `--bin` it builds
`cargo build --release -p sqler-app --locked` with the `features` line of `dist-workspace.toml` (and refuses
`driver-duckdb`). It needs `hyperfine` on `PATH`, prints `key=value` lines ending in `result=pass|breach|failure`, and
writes `bench-<date>-<commit7>-<os>.json` into `--out` (default `target/bench-results`). Exit codes: 0 within budget,
1 a breach or a failed measurement (the report is still written), 2 usage error.

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
