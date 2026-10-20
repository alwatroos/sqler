# Schema catalog

`sqler-catalog` (L2) is the per-connection schema cache that the object browser (T10) and later completion read. Decision and policy: [ADR-0014](../adr/0014-catalog-cache-and-background-refresh.md). It builds on the lazy `Introspect` trait of [ADR-0008](../adr/0008-object-model-lazy-introspection-value-types-tls.md). Numbers: [benchmarks](../benchmarks.md#catalog-tree).

## Parts

```text
  UI / completion                 sqler-catalog                       driver
  ---------------   clone   +---------------------+
  Catalog handle  --------> | Store (RwLock)      | <-- apply (one write)
   cached()  ---- read ---> |  lists  tables      |
   children()/table() miss  |  errors             |
        |                   +---------------------+
        | Load / Refresh              ^
        v  (tokio::sync channels)     | check, write
  +-----------------------------------+--------+
  | CatalogWorker::run()  owns the session     | --> Introspect::list / table
  | one call at a time, loads before refresh   |
  +--------------------------------------------+
```

| Type | Role |
| ---- | ---- |
| `Catalog` | cloneable, `Send + Sync` handle |
| `CatalogWorker` | `Send + 'static` future; `run()` returns the `IntrospectSource` |
| `NodeKey` | parent `ObjectPath` plus `ObjectKind`: the children of one tree node |
| `CacheKey` | `Children(NodeKey)` or `Table(DbObject)` |
| `RefreshScope` | `Node(CacheKey)`, `Subtree(ObjectPath)` or `All` |
| `RefreshHandle` | event channel and cancel for one refresh |
| `CatalogError` | `Driver`, `Cancelled`, `Closed`, `NoIntrospection` |

## Read flow

1. `cached(&key)` and `cached_table(&obj)` look in the store and return an `Arc` or `None`. They are synchronous and never reach the worker or the database.
2. `children(key)` and `table(obj)` do the same check. On a hit the future is ready. On a miss they send a `Load` to the worker and await its reply.
3. The worker re-checks the store before it calls `list` or `table`, so two views asking for the same node cost one call. A failure is returned, recorded for `node_error` and not cached; the next request retries.

## Refresh flow

1. `refresh(scope)` returns a `RefreshHandle` at once. The job lists the keys in scope that are already cached (`Node` always runs its key), so a refresh stays lazy.
2. Events: `Started { total }`, then `Progress { done, total }` after each call and `NodeFailed { key, error }` for a failing node, then `Finished` as the last event: `Applied { refreshed, failed }`, `Cancelled` or `Closed`.
3. Between calls the worker serves queued loads and checks the `CancelFlag` (`cancel()` or `cancel_flag()`). A call already running finishes and its result is dropped.
4. Results are staged. At the end they are applied in a single write: successes replace the old values and clear their errors, failures keep the old data, and objects that disappeared from a re-fetched listing are evicted with everything below them. Until then readers see the previous snapshot. A cancel leaves the store unchanged.
5. Dropping the handle lets the refresh finish.

## Usage from the app

The crate spawns nothing. Spawn the worker on the tokio runtime, never on the GPUI thread (ADR-0007):

```rust,ignore
let (catalog, worker) = Catalog::new(session);
let task = handle.spawn(worker.run());           // tokio Handle

// Any thread: synchronous cache reads while drawing.
let rows = catalog.cached(&key);

// GPUI side: run the future on the runtime and await a oneshot with cx.spawn (SessionBridge).
let fut = catalog.children(key);                  // Send + 'static
handle.spawn(async move { tx.send(fut.await) });

// Close: take the session back and close it.
catalog.shutdown();
let source = task.await?;
```

## Limits

- One session serves loads and refreshes, so a cold load waits at most one introspection call of a running refresh, and a cancel takes effect after the running call.
- Search index and dependency graph are M3. DDL text (`definition`) is not cached.
