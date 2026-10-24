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

See [AGENTS.md](../AGENTS.md) for the project handbook.
