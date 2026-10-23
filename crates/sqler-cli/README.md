# sqler-cli

Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

Status: `run` executes one SQL statement and prints the result as TSV; `export` streams the result of one read statement into a `CSV`, `JSON`, `SQL` or Markdown file (see the user guide for flags, exit codes and Ctrl+C). `mcp` serves the given databases to an AI agent over `MCP` (stdio, or streamable HTTP on `127.0.0.1` with a bearer token).

Layer: L6

## Usage

```sh
cargo run -p sqler-cli -- run :memory: "select 1 as a, 'x' as b"
cargo run -p sqler-cli -- run ./data.db "select * from users"
cargo run -p sqler-cli -- run --driver sqlite ./data.db "select 1"
cargo run -p sqler-cli -- run --driver postgres postgres://user@localhost/db "select 1"
cargo run -p sqler-cli -- run --driver mysql "mysql://user@localhost:3306/db" "select 1"
cargo run -p sqler-cli -- export --format csv --output users.csv ./data.db "select * from users"
cargo run -p sqler-cli -- mcp ./data.db
cargo run -p sqler-cli -- mcp --http 0 --token-file ./mcp.token ./data.db
```

- stdout: a header line of column names, then one tab-separated line per row. `NULL` prints as `NULL`, bytes as `0x..`; backslash, tab, newline and carriage return are escaped as `\\`, `\t`, `\n`, `\r`.
- stderr: a summary such as `(3 rows)`, `(2 rows affected)` or `(done)`, and errors as `error: <message>`.
- Exit codes: `0` success, `1` runtime error, `2` usage error; `export` also uses `3` (refused by the write gate), `130` (cancelled with Ctrl+C) and `143` (terminated by `SIGTERM`, Unix). A second signal while `export` winds down exits at once with the same code.

`mcp [--stdio | --http[=PORT]] [--token-file <PATH>] ([--driver <ID>] <TARGET>)...` names the databases `db1..dbN`. Stdio is the default and prints nothing but protocol bytes on stdout. `--http` binds `127.0.0.1` only (port `0` picks a free one) and needs a bearer token: `SQLER_MCP_TOKEN`, or `--token-file` (read if it exists, else created with mode 0600), or, with neither, a generated token printed once on stdout in the start block. No approver is attached, so only plain reads run; everything else is refused with `no_approver`. `ai.toml` (limits, redaction) is read from the config directory, a malformed one stops the server, and calls are audited to `ai-audit.jsonl` there. Targets never appear in errors or logs. Ctrl+C stops the server.

The `driver-sqlite` feature (on by default) registers the `SQLite` driver. A missing database file is created.

The `driver-postgres` feature (on by default) registers the `PostgreSQL` driver. The target is a `postgres://` URL; TLS follows its `sslmode` parameter (default `prefer`).

The `driver-mysql` feature (on by default) registers the `MySQL` driver for `MySQL` and `MariaDB`. The target is a `mysql://` or `mariadb://` URL; TLS follows its `ssl-mode` parameter (default `preferred`).

The `plugins` feature (off by default) adds the drivers of plugins found in `<config>/plugins` (ADR-0061). The CLI uses the approvals stored in `plugin-grants.toml` and the plugins switched off in `plugin-prefs.toml`, never asks for an approval, and takes no `--plugin-dir`. A driver that is disabled, not approved or needs an engine this build lacks fails with a fixed text that names the plugin and the driver.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
