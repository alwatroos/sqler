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
- `--open-sessions`: after startup, open every saved connection through the connect pipeline, log one `sqler: open-sessions: ...` line per connection on stderr, then idle (used to measure idle RSS with sessions open). Connections with a keychain secret, or an unknown or changed SSH host key, are skipped; SQLer itself never prompts (an SSH agent may) and no secret is printed. A skip line holds the connection name and, for SSH failures, the jump host and port, never the database target, credentials or key material. Cannot be combined with `--exit-after-first-frame`.

Exit codes: `0` normal exit, `1` startup failure (runtime or driver registry), `2` usage error.

The `driver-sqlite` feature (on by default) registers the `SQLite` driver.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
