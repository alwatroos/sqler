# 0008 Object model, lazy introspection, typed values and TLS options

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

M2 adds the object browser (databases, schemas, tables, views, functions, triggers, roles and more), DDL generation and the network drivers. The M0 contract cannot carry that: `Introspect` loads whole schemas, `Table` has no column details, temporal and decimal values are returned as `Text` (ADR-0004), and `ConnectionConfig` has no TLS options. `sqler-core` must stay pure and small, and drivers must stay behind one dyn-compatible contract. AGENTS.md §8 requires an ADR before a new public API.

## Decision drivers

- The browser loads one node at a time; a server with thousands of objects must not be read up front.
- A database that lacks a kind (SQLite has no roles) is not an error.
- `sqler-core` keeps `thiserror` as its only dependency.
- `PostgreSQL` `numeric` has up to 131072 digits before the point; fixed-width decimals cannot hold it.
- Existing `Value` matches in the CLI and the grid must keep compiling.

## Considered options

1. Keep eager `schemas()`/`tables()` and add more eager methods.
2. A lazy, per-node trait with an explicit `Unsupported` result.
3. `chrono` and `rust_decimal` for temporal and decimal values.
4. Small validated std-only value types in `sqler-core`.

## Decision outcome

Chosen: options 2 and 4.

**Object model** (`sqler-core::catalog`). `ObjectKind` has 13 kinds: database, schema, table, view, materialized view, function, procedure, trigger, sequence, type, index, extension and role, with kebab-case ids and an `ObjectLevel` (server, database, schema) that says where the parent of a `list` call points. `ObjectPath` (database, schema, table) addresses a node. `DbObject` is a kind, a parent path and a name; `child_path` gives the path of its children. `Table` carries `ColumnInfo` (type, native type, nullability, default expression), an optional `PrimaryKey` and `ForeignKey`s with `ReferentialAction`. An empty `referenced_columns` means the referenced table's primary key. Everything is `#[non_exhaustive]` with constructors and `with_*` builders, except `Listing`, which has exactly two states.

**Lazy introspection** (`sqler-driver-api::Introspect`).

- `supports(kind)` is synchronous.
- `list(parent, kind)` returns one level of children as `Listing::Objects`, or `Listing::Unsupported` if and only if `!supports(kind)`. It never returns an error for a missing kind.
- A `None` database or schema in `parent` means the session's current or default one.
- For `Index` and `Trigger`, `parent.table` narrows the list to one table and the returned objects carry it when known.
- `table(obj)` accepts `Table`, `View` and `MaterializedView`; other kinds and missing objects give `Ok(None)`.
- `definition(obj)` returns the server-side text (view body, function source, `CREATE` statement) or `Ok(None)` when the server has none.
- Results are ordered by name, except schemas, which keep the server's order.

**Value types** (`sqler-core::value`). `Value` gains `Decimal`, `Date`, `Time`, `Timestamp`, `TimestampTz` and `Json`; `DataType` gains `TimestampTz`. The temporal types and `Decimal` are validated structs with private fields, `Display` and `FromStr`. `Date` is proleptic Gregorian; `Time` has nanosecond precision; `TimestampTz` is the local wall clock plus a UTC offset in seconds. `Decimal` keeps validated text (no exponent), so the scale survives and `"1.50" != "1.5"`. `Json` is the raw server text and is not validated. There is no variant for time with zone (`timetz`). Any value that does not fit a typed variant (`infinity`, `NaN`, a leap second, an unparsable string) falls back to `Text`, so a driver never fails a query because of a cell.

**TLS** (`sqler-core::connection`). `TlsMode` is `disable`, `prefer`, `require`, `verify-ca` or `verify-full`; `TlsConfig` adds optional CA, client certificate and client key paths. `ConnectionConfig::tls` is an `Option`: `None` means the driver follows the target's own parameters (such as `sslmode=`) and otherwise defaults to `prefer`; `Some` is explicit and overrides them. Paths are not secrets, so `Debug` shows them while the target stays redacted. Key passphrases are not supported in M2.

**Removed.** `TableKind`, `Schema` and `Catalog` from M0 are removed. Nothing outside `sqler-core` used them, and the crates are unpublished. The cache is the job of `sqler-catalog` (T7). `CoreError` gains `InvalidValue { kind, input }`.

**SQLite and JSON/DECIMAL.** `sqlx-sqlite` 0.9.0 reports declared `JSON` and `DECIMAL`/`NUMERIC` column types as unknown, so the SQLite driver cannot tell them from the column metadata. Those cells stay `Text`, `Int` or `Float` in results. Table descriptions still map the declared names to `Json` and `Decimal`.

## Consequences

- Good: the browser loads one node per call; unsupported kinds are a plain value, not an error path.
- Good: no new dependency; `sqler-core` still depends only on `thiserror`.
- Good: `Decimal` holds any `PostgreSQL` `numeric`.
- Bad: `sqler-core` has its own date arithmetic validation (month lengths, leap years) to maintain.
- Bad: no calendar arithmetic or time zone conversion on these types; consumers that need it convert at the edge.
- Bad: `Value` and `DataType` grow, so downstream `match` arms need a wildcard (already required by `#[non_exhaustive]`).
- Neutral: the `Table::new` signature changes; the M0 types are gone.

## Pros and cons of the options

- Eager methods: simple, but loads whole catalogs and cannot express an unsupported kind. Rejected.
- Lazy per-node trait: chosen, see above.
- `chrono` and `rust_decimal`: familiar and feature-rich, but add two dependencies to the pure L0 crate, and `rust_decimal`'s 28-digit limit cannot hold `PostgreSQL` `numeric`. Rejected.
- Std-only validated types: chosen; small, enough for display, sorting and round trips.
