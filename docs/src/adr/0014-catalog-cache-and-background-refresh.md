# 0014 Catalog cache, worker concurrency and background refresh

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

The object browser (T10) and later completion read one schema cache per connection. `Introspect` (ADR-0008) takes `&mut self`, so only one call can run at a time on a session. The cache must serve cached nodes without a database round-trip, load a node on demand, refresh in the background with progress and cancel, and stay fast on 5,000 or more tables, with no UI stalls. AGENTS.md §4 forbids holding a lock across `.await`, and §10 forbids `tokio::spawn` on the GPUI thread.

## Decision drivers

- A cached read must not wait behind a refresh or a load.
- Two views asking for the same node must cost one round-trip.
- One failing node must not invalidate the rest.
- The crate is L2 and has no UI and no runtime of its own.

## Considered options

1. An async mutex around the session, shared by all callers.
2. One worker future that owns the session, plus a shared store behind a `std::sync::RwLock`.
3. One task per request, each taking the session.

## Decision outcome

Chosen: option 2.

**Cache model.** The store has three maps: `Children(NodeKey)` to `Arc<Listing>`, `Table(DbObject)` to `Arc<Table>`, and a per-key error. `NodeKey` is a parent `ObjectPath` plus an `ObjectKind`. Paths are not normalised: `None` and `Some("main")` are different keys. `definition()` is not cached. A `table()` result of `None` is not cached. Values are `Arc`, so a reader gets a cheap clone and a refresh swaps whole values.

**Concurrency.** A `Catalog` is a cloneable handle. `CatalogWorker::run()` is a plain `Send + 'static` future that owns the `IntrospectSource` (a `Session` or an `Introspect`) and runs every call in sequence. Handles talk to it over `tokio::sync` channels (`mpsc`, `oneshot`). Reads (`cached`, `children` on a hit) lock the store for a short synchronous section and never reach the worker. On a miss, `children` sends a `Load`. The worker re-checks the store before each load, so concurrent misses on the same node cost one call. The lock is never held across `.await`; `Arc`s are prepared before the write lock is taken. A poisoned lock is recovered with `PoisonError::into_inner`. The crate spawns nothing: the app runs `worker.run()` with the tokio `Handle` (ADR-0007). When the worker is gone, requests fail with `Closed`.

**Refresh policy.**

- Scope: `Node(CacheKey)`, `Subtree(ObjectPath)` or `All`. `Node` runs even when the key is not cached. `Subtree` and `All` re-fetch only keys that are cached when the job starts, so a refresh stays lazy. A key is in a subtree when every `Some` field of the path equals the same field of the key's path; `Subtree(root)` equals `All`.
- A job runs one introspection call at a time. Between calls the worker serves queued loads first (a cold load waits at most one call) and checks the job's `CancelFlag`.
- Events go over an unbounded channel: `Started { total }`, `Progress { done, total }`, `NodeFailed { key, error }`, then `Finished` as the last event (`Applied { refreshed, failed }`, `Cancelled` or `Closed`). Dropping a `RefreshHandle` detaches the job; it does not cancel it.
- Results go to a staging area and are applied in a single write at the end, so readers keep the previous snapshot until then. A cancel drops the staging; the store is unchanged. Cancel is cooperative: a call that is running finishes and its result is discarded.
- Pruning: when a re-fetched `Children` listing no longer contains an object of a container kind (database, schema, table, view, materialized view), every key within that object's `child_path` is evicted. A refreshed `Table` key whose `table()` now returns `None` is removed.

**Errors.** Errors are per key. A failed load is returned, recorded for `node_error` and not cached; the next request retries and a success clears the error. A failed refresh node emits `NodeFailed`, keeps the old data and does not stop the rest. `DriverError::Cancelled` maps to `CatalogError::Cancelled`, any other `DriverError` to `Driver(text)`, and a source without introspection to `NoIntrospection`.

**Shutdown.** `shutdown()`, or dropping every handle, ends the worker. Every pending job gets `Finished(Closed)` and `run()` returns the source so the app can close the session.

## Consequences

- Good: cached reads cost one read lock and an `Arc` clone (about one timer tick in the bench), whatever the worker is doing.
- Good: no duplicate introspection; per-node errors; atomic, cancellable refresh.
- Good: no runtime dependency beyond `tokio::sync`; tests need no Docker.
- Bad: the apply is one write lock over all nodes of the job (about 5,000 inserts for `All`).
- Bad: cancel latency is one introspection call; a cold load can wait for one call of a running refresh.
- Bad: one session serves both loads and refreshes, so a slow server call delays everything behind it.
- Neutral: unseen nodes are never refreshed; the browser loads them when opened.

## Pros and cons of the options

- Async mutex around the session: simple, but holds a lock across `.await` (AGENTS.md §4) and makes cached reads wait behind a refresh. Rejected.
- Worker plus `RwLock` store: chosen, see above.
- One task per request: still needs a lock on the `&mut self` session, so it has the problems of option 1 and gives no ordering for loads versus refresh. Rejected.
