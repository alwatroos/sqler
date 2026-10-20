# 0004 Driver API: boxed futures, event stream, session cancellation

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

SQLer needs a first public API so a database driver (SQLite in T5, others in M2) can be implemented against it and the CLI can run a query without knowing which database is behind it. The domain types live in `sqler-core` (no async, no I/O, no UI). The `Driver` abstraction lives in `sqler-driver-api` and depends only on `sqler-core` among workspace crates. AGENTS.md §10.2 requires an ADR before a new public API.

## Decision drivers

- Dyn-compatible: the CLI holds `Box<dyn Driver>` and `Box<dyn Session>`.
- Cancellable: every long-running operation can be cancelled from another task.
- Streaming: results arrive in batches, not as one materialized set.
- Minimal dependencies: only `thiserror` is added.

## Considered options

1. Native `async fn` in traits.
2. `async-trait`.
3. `futures-core::Stream` as the result type.
4. Boxed futures (`BoxFuture`) plus a `QueryEvent` stream.

## Decision outcome

Chosen: option 4, boxed futures plus an event stream.

- `Driver::connect` and `Session::execute` return `BoxFuture`, so the traits are object-safe without a macro.
- `execute` returns a `Box<dyn QueryStream + 's>`, which borrows the session. `QueryStream::next_event` yields `QueryEvent`: one `Columns`, zero or more `Rows`, one `Done`, then `None`. An `Err` ends the stream. `collect` enforces this protocol and builds a `ResultSet`.
- Cancellation: `Session::cancel_handle` returns a `CancelHandle` (backed by a `Canceller`) taken before `execute`. After a cancel the stream yields `Err(DriverError::Cancelled)` and then `None`. `CancelFlag` is a cooperative building block for drivers without an async interrupt.
- Introspection is an optional `Introspect` trait reached through `Session::introspection`, which returns `None` by default.
- Errors are typed: `CoreError` and `DriverError` via `thiserror`.
- Public structs and enums that may grow are `#[non_exhaustive]` and have constructors.

## Consequences

- Good: dyn-compatible, runtime-agnostic, one new dependency (`thiserror`), tests need no async runtime.
- Good: drivers that use `futures` internally (such as sqlx) can adapt easily.
- Bad: one heap allocation per future; acceptable for database round-trips.
- Bad: `#[non_exhaustive]` forces downstream code to use constructors and wildcard match arms.
- Neutral: temporal and decimal `Value` variants are deferred to M2; drivers return them as `Text` for now.

## Pros and cons of the options

- Native `async fn` in traits: no allocation, but not dyn-compatible. Rejected.
- `async-trait`: dyn-compatible, but adds a proc-macro dependency for something plain boxing does. Rejected.
- `futures-core::Stream`: familiar, but adds a dependency and needs `StreamExt` for `next()`. Rejected.
- Boxed futures plus event stream: chosen, see above.
