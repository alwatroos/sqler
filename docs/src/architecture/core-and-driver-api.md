# Core and driver API

Decision records: [ADR-0004](../adr/0004-driver-api.md) (driver API) and [ADR-0008](../adr/0008-object-model-lazy-introspection-value-types-tls.md) (object model, lazy introspection, typed values, TLS).

## Layering

- L0 `sqler-core`: plain, synchronous domain types and `CoreError`. No async, no I/O, no UI.
- L1 `sqler-driver-api`: the `Driver`, `Session`, `QueryStream` and `Introspect` traits, the registry and cancellation helpers. Depends only on `sqler-core` among workspace crates.
- Drivers (T5 and later) and the CLI depend on L1. The CLI sees only `dyn` traits.

## Type inventory

`sqler-core`:

- `CoreError`, `DriverId`, `ConnectionConfig`, `Query`
- `TlsMode`, `TlsConfig`: connection security options, carried in `ConnectionConfig::tls`; `TlsConfig::server_name` is the name to verify and send as SNI when it differs from the dial host (ADR-0019)
- `DataType`, `Value`, `Column`, `Row`, `ResultSet`
- `Date`, `Time`, `Timestamp`, `TimestampTz`, `Decimal`: validated value types, std only
- `ObjectKind`, `ObjectLevel`, `ObjectPath`, `DbObject`, `Listing`: the object model
- `Table`, `ColumnInfo`, `PrimaryKey`, `ForeignKey`, `ReferentialAction`: table details

`sqler-driver-api` (re-exports `sqler_core`):

- `BoxFuture`, `Driver`, `Session`
- `QueryEvent`, `QueryStream`, `collect`
- `Canceller`, `CancelHandle`, `CancelFlag`
- `Introspect`
- `DriverError`, `DriverRegistry`

## Session lifecycle

1. Register drivers in a `DriverRegistry` (`register`). Ids are validated by `DriverId::new`; duplicates return `DuplicateDriver`.
2. `DriverRegistry::connect(&ConnectionConfig)` picks the driver by `config.driver` (`UnknownDriver` if absent) and returns a `Box<dyn Session>`.
3. Take `Session::cancel_handle` before executing.
4. `Session::execute(&Query)` returns a `Box<dyn QueryStream + 's>`, which borrows the session.
5. Read `QueryEvent`s with `next_event`, or call `collect` to build a `ResultSet`.
6. `Session::close` ends the session.

## Protocol rules

- Exactly one `Columns` first (an empty vec for statements that return no rows), then zero or more `Rows`, then exactly one `Done`, then `None` forever.
- An `Err` ends the stream.
- Dropping the stream must stop fetching.
- `collect` returns `DriverError::Protocol` for `Rows` before `Columns`, a second `Columns`, or `None` before `Done`. A width mismatch returns `DriverError::Core`. Stream errors pass through unchanged.

## Cancellation

`CancelHandle` wraps an `Arc<dyn Canceller>` and is `Clone`, so another task can call `cancel` while the stream is being read. After a cancel the stream yields `Err(DriverError::Cancelled)` and then `None`. `CancelFlag` is a cooperative implementation: the driver checks `is_cancelled` between batches. It suits `SQLite`, which has no async interrupt in sqlx.

## Values

`Value` has the variants `Null`, `Bool`, `Int`, `Float`, `Text`, `Bytes` and the typed `Decimal`, `Date`, `Time`, `Timestamp`, `TimestampTz` and `Json`. The temporal and decimal types are small validated structs with private fields; they implement `Display` and `FromStr` and need no external crate. `Decimal` keeps its text and scale, so `1.50` and `1.5` are different values. `Json` holds the raw server text and is not validated.

A value that does not fit a typed variant (`infinity`, `NaN`, a leap second, a time with zone) falls back to `Text`, so a driver never fails a query because of an exotic value.

## Introspection

`Introspect` is reached through `Session::introspection`, which returns `None` by default. It is lazy: each call loads one node, never the whole catalog.

- `supports(kind)` says synchronously whether the database has that object kind.
- `list(parent, kind)` returns the objects of one kind under `parent` (an `ObjectPath`). It returns `Listing::Unsupported`, not an error, if and only if `supports(kind)` is false.
- `table(object)` returns the columns, primary key and foreign keys of a table, view or materialized view, or `None` for another kind or a missing object.
- `definition(object)` returns the server-side `CREATE` text, or `None` if the server has none.

Rules: a `None` database or schema in the parent means the session's current or default one; for `Index` and `Trigger`, `parent.table` narrows the list to one table; results are ordered by name, except schemas, which keep the server's order. `ObjectKind::level` tells which part of `ObjectPath` a kind's `list` call needs.

The `SQLite` driver is the reference implementation, see [SQLite](../user-guide/databases/sqlite.md).

## How to implement a driver

1. Implement `Driver` with an `id` that satisfies `DriverId::new` (`[a-z0-9-]`, non-empty).
2. Implement `Session`: return a `CancelHandle` from `cancel_handle`, a stream from `execute`, and release resources in `close`.
3. Implement `QueryStream` following the protocol rules, including the `Cancelled` then `None` ending.
4. Check the `CancelFlag` (or a native interrupt) between batches.
5. Map backend errors to `DriverError` (`Backend` for the source error).
6. Return the typed `Value` variants for temporal, decimal and JSON columns, and fall back to `Text` for values that do not fit.
7. Optionally implement `Introspect` following the rules in [Introspection](#introspection), and return it from `Session::introspection`. `list` must answer `Listing::Unsupported` for exactly the kinds `supports` rejects.
8. Register the driver in a `DriverRegistry`.
