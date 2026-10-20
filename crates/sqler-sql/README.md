# sqler-sql

SQL parsing (`sqlparser`), formatting, statement splitting and dialect handling.

Status: M2: DDL generation (parsing, formatting and splitting follow in M3)

Layer: L1

API (pure, no I/O): `Dialect`, `Script`, `quote_identifier`, `qualified_name`, `supports`,
`create_table`, `create_script`, `select_star`, `insert_template`, `drop_script`, `DdlError`,
`DEFAULT_ROW_LIMIT`. Dialects: `PostgreSQL`, `MySQL`/`MariaDB`, `SQLite`. Every identifier is
quoted. See [ADR-0015](../../docs/src/adr/0015-ddl-generation-dialects-and-quoting.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
