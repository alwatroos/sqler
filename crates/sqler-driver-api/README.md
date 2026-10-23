# sqler-driver-api

The `Driver` trait, streaming result protocol, cancellation and introspection trait that every database driver implements.

Status: API v0 (M0)

Object-safe traits (`Driver`, `Session`, `QueryStream`, `Introspect`) built on a local `BoxFuture`,
so the CLI can hold `Box<dyn Driver>` without knowing the backend.

Stream protocol: one `Columns` (empty for statements without rows), zero or more `Rows`, one
`Done`, then `None`. An `Err` ends the stream. `collect` gathers a stream into a `ResultSet` and
enforces this. Cancellation: take `Session::cancel_handle` before `execute`; after a cancel the
stream yields `Err(DriverError::Cancelled)` and then `None`. `CancelFlag` is the cooperative
building block for drivers such as `SQLite`. `DriverRegistry` maps driver ids to drivers; a `DriverSource` adds run-time drivers (plugins) after the built-ins.

Layer: L1

See [AGENTS.md](../../AGENTS.md) for the project handbook.
