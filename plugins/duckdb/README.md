# DuckDB plugin

**Not in 1.0** (ADR-0070, amendment 2). This plugin and its engine stay in the repository, disabled; they are not part
of any release artifact, installer or update manifest, and CI builds the engine only in the separate `duckdb` job.

A declarative shell: `sqler.toml` declares one host driver (`id = "duckdb"`, `engine = "duckdb"`) and the capability
`driver.host:duckdb`. There is no `[entry]` and no component. The engine itself is the crate
`crates/sqler-driver-duckdb` (ADR-0070), linked into the binary only when it is built with the `driver-duckdb`
feature of `sqler-app` or `sqler-cli`. Both features are off by default and imply `plugins`.

```
cargo build --release -p sqler-app --features driver-duckdb
cargo run -p xtask -- plugins build duckdb      # stages target/plugins/<debug|release>/duckdb/sqler.toml
```

Install by copying the staged `sqler.toml` into `<config>/plugins/duckdb/` and approve `driver.host:duckdb` in the
Plugins view. Without the feature the driver is listed as unavailable ("needs the `duckdb` engine, which this build
does not include").

The `dialect = "duckdb"` key is a hint only; the host has no consumer for it yet (see "API gaps" in
`docs/src/plugin-development/drivers.md`). The sandbox (no external file access, no extensions) is engine
configuration, not a grant.
