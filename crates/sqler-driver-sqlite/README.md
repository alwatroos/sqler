# sqler-driver-sqlite

`SQLite` driver implementation.

Status: implemented (M0)

Layer: L2

Implements the `sqler-driver-api` `Driver`, `Session` and `QueryStream` traits on `sqlx`'s bundled
`SQLite` backend. One connection per session; no pool, so `:memory:` is private to its session.

## Usage

```rust,ignore
let mut registry = sqler_driver_api::DriverRegistry::new();
registry.register(Box::new(sqler_driver_sqlite::SqliteDriver::new()))?;
```

The `ConnectionConfig::target` is `:memory:`, a `sqlite:` URL, or a file path. A missing file is
created.

## Runtime

`sqlx` runs on Tokio: the futures returned by the driver must be polled inside a Tokio runtime
context (for example a current-thread runtime built with `enable_all`).

## Cancellation

Cancellation is cooperative. The cancel flag is reset at each `execute` and checked before every
batch of 256 rows; after a cancel the stream yields `Err(Cancelled)` and then ends. A single long
step before the first row (for example a large sort) cannot be interrupted.

See [AGENTS.md](../../AGENTS.md) for the project handbook.
