# Command-line interface

`sqler-cli` is the headless front end of SQLer. It connects to a database through a driver, runs one
SQL statement and prints the result. It does not depend on the UI crates.

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

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | Success, or `--help` / `--version` |
| 1 | Runtime error (unknown driver, connection failure, bad SQL); `error: <message>` on stderr |
| 2 | Usage error; `error: <message>` and the usage text on stderr |

## Cancellation

`PostgreSQL` cancels on the server: the running statement is stopped and the stream ends with a
cancelled error. The `SQLite` driver cancels cooperatively: a cancel request is checked between
batches of rows. A single step that produces no rows for a long time, such as sorting a very large
table before the first row, cannot be interrupted. The CLI has no Ctrl-C handler; the process exits on a signal.
