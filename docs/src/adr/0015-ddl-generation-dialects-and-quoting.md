# 0015 DDL generation: dialects and quoting

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The object browser (M2) offers `CREATE`, `SELECT *`, an `INSERT` template, `DROP` and "copy name" for every object. The scripts must be correct for `PostgreSQL`, `MySQL`/`MariaDB` and `SQLite`, including names with quotes, spaces, reserved words and mixed case. `sqler-sql` is an L1 crate that depends on `sqler-core` only, so it cannot talk to a server. AGENTS.md §8 requires an ADR before a new public API.

## Decision drivers

- A generated script must never be able to break out of an identifier, whatever the object is called.
- The functions must be pure: no I/O, no async, easy to test per dialect and kind.
- No per-dialect or per-version keyword list to maintain.
- The same `ObjectKind` model as ADR-0008 (`DbObject`, `Table`).

## Considered options

1. Quote an identifier only when needed (reserved word, unusual characters).
2. Always quote every identifier and double the quote character.
3. Query the server for its own `CREATE` text for every kind, tables included.
4. Render tables from the model; use server text for other kinds, supplied by the caller.

## Decision outcome

Chosen: options 2 and 4.

**Dialect.** `Dialect` is `Postgres`, `MySql` or `Sqlite` (`#[non_exhaustive]`; `MSSQL` is not in scope). `as_str` returns the driver id (`postgres`, `mysql`, `sqlite`); `from_driver_id` is an exact match on it. `MySql` covers `MariaDB`, which shares the driver id `mysql`. `Dialect::ALL` lists all three.

**Public API** (all pure, all re-exported from `sqler_sql`). Functions return `Result<String, DdlError>`:

- `quote_identifier(dialect, name)`
- `qualified_name(dialect, object)`: the "copy name"
- `supports(dialect, kind, script) -> bool`
- `create_table(dialect, table, index_definitions)`
- `create_script(dialect, object, definition)`
- `select_star(dialect, object, limit)`
- `insert_template(dialect, table)`
- `drop_script(dialect, object)`

`Script` is `Create`, `SelectStar`, `InsertTemplate` or `Drop`. `DEFAULT_ROW_LIMIT` is 1000; the caller passes the limit. `DdlError` is `InvalidIdentifier`, `Unsupported`, `MissingTable`, `EmptyTable` or `MissingDefinition`.

**Support matrix.** `Create` and `Drop` share one matrix:

| Dialect | Kinds |
| ------- | ----- |
| `Postgres` | all 13 kinds |
| `MySql` | database, schema, table, view, function, procedure, trigger, index, sequence |
| `Sqlite` | table, view, trigger, index |

`SelectStar` covers table and view, plus materialized view on `PostgreSQL`. `InsertTemplate` covers table only. Any other combination returns `DdlError::Unsupported`; every function checks `supports` before it builds text.

**Always quote.** Every identifier goes through `quote_identifier`. `PostgreSQL` and `SQLite` wrap it in `"` and double each `"`; `MySQL` does the same with a backtick. Nothing else is changed, so Unicode and whitespace are kept. An empty name or one containing NUL is rejected with `InvalidIdentifier` and is never cleaned up silently. A name is never interpolated into a script in any other way.

**Definition text comes from the caller.** Views, functions, procedures, triggers and the other non-table kinds use the server's text (`Introspect::definition`), passed in by the caller. Index definitions for a table `CREATE` are passed the same way, because the model has no index columns. The crate does no I/O. Tables are rendered from the model, even when a server text exists.

**Verbatim text.** Definition text, column `DEFAULT` expressions and `native_type` are emitted verbatim. They are server catalog text, not user input, and are not escaped. Only identifiers are safe by construction.

**No `IF EXISTS`, no `CASCADE`.** `DROP` is plain. A missing object should fail loudly, and `CASCADE` would silently remove dependent objects the user has not seen. Executing a `DROP` and its confirmation belong to T10.

**Dev-dependencies.** `insta` (snapshots per dialect and kind) and `proptest` (quoting properties) are dev-dependencies of `sqler-sql` only and do not enter the release binary. Their versions are in `docs/src/dependencies.md`.

Two grammar rules refine the generic form. A `SQLite` foreign key writes `REFERENCES "table"` without the schema, because the `SQLite` grammar does not allow one and its driver always reports the schema `main`. A table without columns renders `CREATE TABLE x ();` on `PostgreSQL` and is `DdlError::EmptyTable` on `MySQL` and `SQLite`, which cannot create it. An integration test in `sqler-driver-sqlite` runs the generated `SQLite` scripts against an in-memory database.

## Consequences

- Good: no keyword list, so no reserved-word bug can leak into a script.
- Good: pure functions are tested exhaustively with snapshots and properties.
- Good: `sqler-sql` still depends only on `sqler-core` among workspace crates.
- Bad: output is more verbose than hand-written SQL (`"public"."users"`).
- Bad: `PostgreSQL` `DROP FUNCTION name` fails for overloaded functions, because the model has no argument list.
- Bad: `MySQL` routine definitions contain inner `;`; running them needs statement splitting (M3).
- Bad: `MySQL` roles and users (`'u'@'h'`) are unsupported in M2.
- Neutral: the caller must fetch definitions and index texts before it calls the generators.

## Pros and cons of the options

- Quote only when needed: nicer output, but it needs a reserved-word list per dialect and version, and a missing entry is a correctness and safety bug. Rejected.
- Always quote: chosen; trivially correct and provable by a round-trip property.
- Server text for tables: the text differs per server and version and cannot be built from the model's columns, defaults and keys. Rejected for tables.
- Model for tables, server text for the rest: chosen; tables are fully described by the model, other kinds are not.
