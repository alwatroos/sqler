# sqler-driver-duckdb

`DuckDB` driver implementation.

Status: implemented (M6, P15)

Layer: L2

Unsafe: none (`#![forbid(unsafe_code)]`).

Implements the `sqler-driver-api` `Driver`, `Session`, `QueryStream` and `Introspect` traits on the `duckdb` crate with
the bundled engine. It is the native engine behind the declarative `duckdb` plugin shell (`plugins/duckdb`): the host
registers it under the engine id `duckdb` behind the default-off feature `driver-duckdb` of the binaries (ADR-0061,
ADR-0070).

## Usage

```rust,ignore
let mut registry = sqler_driver_api::DriverRegistry::new();
registry.register(Box::new(sqler_driver_duckdb::DuckDbDriver::new()))?;
```

The `ConnectionConfig::target` is `:memory:` (private to its session) or a file path; a missing file is created. Sessions
on one file share one `DuckDB` instance, so several tabs can use the same file at once.

## Sandbox

External file access, extension install and autoload and community extensions are off, and the configuration is
locked. Error texts never contain the database path (it is replaced by `<database>`).

## Runtime

Each session runs one worker thread that owns the blocking connection. `close` joins it with
`tokio::task::spawn_blocking`, so run it inside a Tokio runtime.

## Cancellation

Cancel sets a flag and interrupts the running statement through `DuckDB`'s interrupt handle, so a long single step
stops too. After a cancel the stream yields `Err(Cancelled)` and then ends; the session stays usable.

## Build cost

The bundled C++ build adds minutes to an uncached build. Tests: `cargo test -p sqler-driver-duckdb` (in-process, no
container).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
