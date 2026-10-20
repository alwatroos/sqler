# sqler-app

The `SQLer` desktop application: the `sqler` binary. It loads `connections.toml`, wires the driver crates into a `DriverRegistry`, starts a tokio runtime for database work and opens the `GPUI` window from `sqler-ui`.

Status: opens the workspace shell (connection sidebar and tab strip). Tab bodies are placeholders until the SQL editor arrives.

Layer: L7. This is the only crate that depends on `sqler-ui`.

## Usage

```sh
cargo run -p sqler-app
cargo run -p sqler-app -- --version
```

Options:

- `-h`, `--help`: print the usage.
- `-V`, `--version`: print `sqler <version>`.
- `--exit-after-first-frame`: exit with code 0 once the first frame is presented (used to measure cold start).

Exit codes: `0` normal exit, `1` startup failure (runtime or driver registry), `2` usage error.

The `driver-sqlite` feature (on by default) registers the `SQLite` driver.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
