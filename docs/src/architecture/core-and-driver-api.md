# Core and driver API

Decision record: [ADR-0004](../adr/0004-driver-api.md).

## Layering

- L0 `sqler-core`: plain, synchronous domain types and `CoreError`. No async, no I/O, no UI.
- L1 `sqler-driver-api`: the `Driver`, `Session`, `QueryStream` and `Introspect` traits, the registry and cancellation helpers. Depends only on `sqler-core` among workspace crates.
- Drivers (T5 and later) and the CLI depend on L1. The CLI sees only `dyn` traits.

## Type inventory

`sqler-core`:

- `CoreError`, `DriverId`, `ConnectionConfig`, `Query`
- `DataType`, `Value`, `Column`, `Row`, `ResultSet`
- `TableKind`, `Table`, `Schema`, `Catalog`

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

## Introspection

Status: shape only. `Introspect` (`schemas`, `tables`, `table`) is reached through `Session::introspection`, which returns `None` by default. No implementations exist in M0.

## How to implement a driver

1. Implement `Driver` with an `id` that satisfies `DriverId::new` (`[a-z0-9-]`, non-empty).
2. Implement `Session`: return a `CancelHandle` from `cancel_handle`, a stream from `execute`, and release resources in `close`.
3. Implement `QueryStream` following the protocol rules, including the `Cancelled` then `None` ending.
4. Check the `CancelFlag` (or a native interrupt) between batches.
5. Map backend errors to `DriverError` (`Backend` for the source error).
6. Return temporal and decimal values as `Text` until M2.
7. Optionally implement `Introspect` and return it from `Session::introspection`.
8. Register the driver in a `DriverRegistry`.
