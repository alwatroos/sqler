# sqler-cli

Headless command-line interface: run a query, export results and start the `MCP` server. Proves the core is UI-independent.

Status: runs one SQL statement against a `SQLite` database and prints the result as TSV. Export formats and the `MCP` server start command are not implemented yet.

Layer: L6

## Usage

```sh
cargo run -p sqler-cli -- run :memory: "select 1 as a, 'x' as b"
cargo run -p sqler-cli -- run ./data.db "select * from users"
cargo run -p sqler-cli -- run --driver sqlite ./data.db "select 1"
```

- stdout: a header line of column names, then one tab-separated line per row. `NULL` prints as `NULL`, bytes as `0x..`; backslash, tab, newline and carriage return are escaped as `\\`, `\t`, `\n`, `\r`.
- stderr: a summary such as `(3 rows)`, `(2 rows affected)` or `(done)`, and errors as `error: <message>`.
- Exit codes: `0` success, `1` runtime error, `2` usage error.

The `driver-sqlite` feature (on by default) registers the `SQLite` driver. A missing database file is created.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
