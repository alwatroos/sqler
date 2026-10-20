# sqler-sql

SQL text services (statement splitting, cursor lookup, placeholders, classification, formatting), DDL generation and dialect handling.

Status: M3: SQL text services on an own lexer (parsing with `sqlparser` is left for later AST needs)

Layer: L1

API (pure, no I/O): `Dialect`, `Script`, `quote_identifier`, `qualified_name`, `supports`,
`create_table`, `create_script`, `select_star`, `insert_template`, `drop_script`, `DdlError`,
`DEFAULT_ROW_LIMIT`. Dialects: `PostgreSQL`, `MySQL`/`MariaDB`, `SQLite`. Every identifier is
quoted. See [ADR-0015](../../docs/src/adr/0015-ddl-generation-dialects-and-quoting.md).

SQL text API (pure, no I/O, never fails on half-typed text): `split`, `statement_at`, `Segment`,
`SegmentKind`, `placeholders`, `Placeholder`, `PlaceholderKind`, `classify`, `classify_each`,
`StatementClass`, `format`, `FormatOptions`, `FormatError`. Classification is fail-safe: `Read` only
when no write syntax is found, so callers that must be sure also use a read-only transaction. See
[ADR-0020](../../docs/src/adr/0020-sql-text-services.md) and the
[architecture page](../../docs/src/architecture/sql.md).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
