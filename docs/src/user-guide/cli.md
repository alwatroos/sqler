# Command-line interface

`sqler-cli` is the headless front end of SQLer. It connects to a database through a driver, runs one
SQL statement and prints the result, or exports it to a file. It does not depend on the UI crates.

Status: the `SQLite` and `PostgreSQL` drivers are available.

## Install

From a checkout of the repository:

```sh
cargo install --path crates/sqler-cli
```

Or run it without installing:

```sh
cargo run -p sqler-cli -- run :memory: "select 1"
```

The `SQLite` driver is compiled in by the default `driver-sqlite` feature. `SQLite` itself is bundled,
so no system library is needed. The `PostgreSQL` driver is compiled in by the default
`driver-postgres` feature, and the `MySQL` and `MariaDB` driver by the default `driver-mysql`
feature. With `--no-default-features` no driver is registered and `run` fails with
`unknown driver`.

## Usage

```text
sqler-cli run [--driver <ID>] <TARGET> <SQL>
sqler-cli export [--driver <ID>] --format <FMT> --output <PATH|-> [--table <NAME>]
                 [--schema <NAME>] [--allow-write] <TARGET> <SQL>
sqler-cli --help | -h
sqler-cli --version | -V
```

- `--driver <ID>` (or `--driver=<ID>`) selects the driver and defaults to `sqlite`. The flag may also
  come after `<TARGET>` and `<SQL>`.
- `<TARGET>` is `:memory:` for a private in-memory database, a `sqlite:` URL, or a file path
  (`SQLite`; a missing database file is created), a `postgres://` URL (`PostgreSQL`), or a `mysql://` or `mariadb://` URL (`MySQL`, `MariaDB`).
- `<SQL>` is one statement.

In-memory database:

```sh
sqler-cli run :memory: "select 1 as a, 'x' as b, NULL as c"
```

```text
a	b	c
1	x	NULL
```

Database file:

```sh
sqler-cli run app.db "create table t (id integer, name text)"
sqler-cli run app.db "insert into t values (1, 'a'), (2, 'b')"
sqler-cli run app.db "select * from t"
```

`PostgreSQL`:

```sh
sqler-cli run --driver postgres postgres://user@localhost/db "select 1"
```

TLS settings go in the URL, for example `?sslmode=verify-full`; see
[PostgreSQL](databases/postgresql.md).

`MySQL` and `MariaDB`:

```sh
sqler-cli run --driver mysql "mysql://user@localhost:3306/db" "select 1"
```

TLS settings go in the URL, for example `?ssl-mode=verify_identity`; see
[MySQL and MariaDB](databases/mysql.md).

Each invocation opens its own connection, so an `:memory:` database does not survive the command.

## Output

Results go to stdout as tab-separated values: a header line with the column names, then one line
per row. Values print as `NULL`, numbers, text, or `0x..` for binary data. In text, the backslash,
tab, newline and carriage return are written as `\\`, `\t`, `\n` and `\r`, so every row stays on one
line.

A summary goes to stderr only, so stdout can be piped:

- `(<n> rows)` for statements that return rows
- `(<n> rows affected)` for statements without result columns that report a count
- `(done)` otherwise

With `SQLite`, statements without result columns always report a count, so DDL such as
`create table` prints `(0 rows affected)` and `(done)` does not appear.

Booleans have no separate storage class in `SQLite`, so they print as integers. Text in a column
declared `DATE`, `TIME`, `DATETIME` or `TIMESTAMP` is decoded into a typed value and prints in
normalised ISO form: `2024-01-02T03:04:05` prints as `2024-01-02 03:04:05`. Text that does not parse
stays as it is. See [SQLite](databases/sqlite.md).

## Export

`export` runs one statement and streams its rows into a file, one batch at a time, so memory does
not grow with the row count. It uses the writers of `sqler-export`.

```sh
sqler-cli export --format csv --output users.csv app.db "select * from users"
sqler-cli export --format json --output - app.db "select * from users" | jq .
sqler-cli export --format sql --table users --output users.sql app.db "select * from users"
```

- `--format <FMT>` is required: `csv`, `json`, `sql` (`INSERT` statements) or `markdown`. A format
  that is not in this build (`parquet`, `xlsx`, when built without the matching `export-parquet` or
  `export-xlsx` feature) is refused with exit code 2 and a message saying so.
- `--output <PATH>` is required. `-` writes to stdout and is allowed for the text formats only; a
  binary format to `-` is a usage error. A short summary such as `(2 rows, 41 bytes) to users.csv`
  goes to stderr.
- `--table <NAME>` is required for `sql`; `--schema <NAME>` adds a schema (or database on `MySQL`).
  The identifiers and literals follow the dialect of `--driver`.
- All flags may appear anywhere, also as `--flag=VALUE`. `--` ends the options: what follows is
  `<TARGET>` and `<SQL>`, so SQL that starts with `-` (a leading `-- comment`) can be passed. `run`
  accepts `--` too.

### Write gate

The statement is classified before the CLI connects. Only a statement that `sqler-sql` classifies as
a read (`select`, `with`, `values`, `table`, `show`) is exported. Anything else, including
statements the classifier does not recognise, is refused with exit code 3 and nothing runs. Pass
`--allow-write` to run it anyway; the CLI then prints
`warning: running a statement of class <class> (--allow-write)` on stderr first. The SQL itself is
never printed, because a redactor cannot guarantee that every secret in it is hidden. The class is `write`, `ddl`, `other` or `unknown`. The classifier cannot see what
a view or function does, and the CLI runs the statement without a read-only transaction. Exactly one
statement is accepted. A statement without a result set (such as an `insert`) writes no file and
prints `(<n> rows affected)`. The gate applies to `export` only; `run` runs any statement.

### Ctrl+C, SIGTERM and partial files

Output to a file goes to a hidden file next to the target (`.<name>.<pid>.partial`), which is
renamed over the target only when the export has finished. If the export fails or is cancelled with
Ctrl+C (`SIGINT`) or `SIGTERM`, the hidden file is deleted and an existing target stays as it was.
Exit code 130 means cancelled by Ctrl+C, 143 by `SIGTERM` (Unix). The handlers are installed before
the CLI connects, so a signal after start-up never leaves the hidden file; a signal during connect
abandons the connection. A process killed with `SIGKILL` can leave the hidden file behind (on
Windows the file is not marked hidden). With `--output -` the output already written stays on
stdout, truncated; the exit code says so. A write error on stdout (a closed pipe, a full disk) fails the export with exit code 1. A stdout that is closed before the process starts (`>&-`) is not an error: the Rust runtime replaces it with `/dev/null` at start-up.

The rename replaces the target as a directory entry. A symbolic link as `--output` is replaced by
a regular file and the file it pointed to is not touched; on Unix a read-only existing target is
replaced too (only the directory must be writable; on Windows a rename over a read-only target can
fail with access denied, not verified); the new file has the default permissions of the
process, not those of the file it replaces.

## MCP server

```sh
sqler-cli mcp [--stdio | --http[=PORT]] [--token-file PATH] ([--driver <ID>] <TARGET>)...
```

Serves the AI tools to Model Context Protocol clients over stdio (default) or loopback HTTP with a bearer
token. Only read statements run. See [MCP server](../ai/mcp.md).

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success, or `--help` / `--version` |
| 1 | Runtime error (unknown driver, connection failure, bad SQL, a file that cannot be written); `error: <message>` on stderr |
| 2 | Usage error (also: unknown or compiled-out format, binary format to stdout, `sql` without `--table`, not exactly one statement); `error: <message>` and the usage text on stderr |
| 3 | `export` only: the statement is not a plain read and `--allow-write` was not given |
| 130 | `export` only: cancelled with Ctrl+C |
| 143 | `export` only: terminated by `SIGTERM` (Unix) |

Error messages never repeat an argument (driver id, format, `--table`, `--schema`, target, SQL) and
the text of a driver error is replaced by a fixed one per kind (connection failed, query failed, ...),
so a URL in the wrong place, such as `--driver postgres://user:pw@host/db`, does not put its password
on stderr. A file error names no path. The cause of a failed query is withheld too: run the statement
in the app to see it. Only the success summary names the output file.

## Cancellation

`export` handles Ctrl+C and, on Unix, `SIGTERM` (see above), also while the connection closes. `run` has no handler; the process exits on a signal.

`PostgreSQL` cancels on the server: the running statement is stopped and the stream ends with a
cancelled error. The `SQLite` driver cancels cooperatively: a cancel request is checked between
batches of rows. A single step that produces no rows for a long time, such as sorting a very large
table before the first row, cannot be interrupted. A second Ctrl+C or `SIGTERM` while `export` is cancelling or closing the connection exits at once
with 130 or 143; the cancel request is sent before the connection is closed, and a hidden file that
is still there at that moment is left behind.
