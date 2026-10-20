# sqler-cli

Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

Status: `run` executes one SQL statement and prints the result as TSV; `export` streams the result of one read statement into a `CSV`, `JSON`, `SQL` or Markdown file (see the user guide for flags, exit codes and Ctrl+C). The `MCP` server start command is not implemented yet.

Layer: L6

## Usage

```sh
cargo run -p sqler-cli -- run :memory: "select 1 as a, 'x' as b"
cargo run -p sqler-cli -- run ./data.db "select * from users"
cargo run -p sqler-cli -- run --driver sqlite ./data.db "select 1"
cargo run -p sqler-cli -- run --driver postgres postgres://user@localhost/db "select 1"
cargo run -p sqler-cli -- run --driver mysql "mysql://user@localhost:3306/db" "select 1"
cargo run -p sqler-cli -- export --format csv --output users.csv ./data.db "select * from users"
```

- stdout: a header line of column names, then one tab-separated line per row. `NULL` prints as `NULL`, bytes as `0x..`; backslash, tab, newline and carriage return are escaped as `\\`, `\t`, `\n`, `\r`.
- stderr: a summary such as `(3 rows)`, `(2 rows affected)` or `(done)`, and errors as `error: <message>`.
- Exit codes: `0` success, `1` runtime error, `2` usage error; `export` also uses `3` (refused by the write gate) and `130` (cancelled).

The `driver-sqlite` feature (on by default) registers the `SQLite` driver. A missing database file is created.

The `driver-postgres` feature (on by default) registers the `PostgreSQL` driver. The target is a `postgres://` URL; TLS follows its `sslmode` parameter (default `prefer`).

The `driver-mysql` feature (on by default) registers the `MySQL` driver for `MySQL` and `MariaDB`. The target is a `mysql://` or `mariadb://` URL; TLS follows its `ssl-mode` parameter (default `preferred`).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
