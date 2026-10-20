# Schema catalog

`sqler-catalog` (L2) is the per-connection schema cache that the object browser (T10) and later completion read. Decision and policy: [ADR-0014](../adr/0014-catalog-cache-and-background-refresh.md). It builds on the lazy `Introspect` trait of [ADR-0008](../adr/0008-object-model-lazy-introspection-value-types-tls.md). Snapshot, name index and column loading: [ADR-0022](../adr/0022-catalog-snapshot-name-index.md). Numbers: [benchmarks](../benchmarks.md#catalog-tree) and [catalog index](../benchmarks.md#catalog-index).

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
| `CatalogSnapshot` | immutable, `Send + Sync` view of the cache with a `generation` |
| `NameQuery`, `NameHit` | scoped, bounded name lookup and its ranked result |
| `TableState` | `NotLoaded`, `Loaded(Arc<Table>)` or `Failed(CatalogError)` |
| `ForeignKeyTarget` | forward foreign key of a loaded table |
| `CatalogError` | `Driver`, `Cancelled`, `Closed`, `NoIntrospection` |

## Read flow

1. `cached(&key)` and `cached_table(&obj)` look in the store and return an `Arc` or `None`. They are synchronous and never reach the worker or the database.
2. `children(key)` and `table(obj)` do the same check. On a hit the future is ready. On a miss they send a `Load` to the worker and await its reply.
3. The worker re-checks the store before it calls `list` or `table`, so two views asking for the same node cost one call. A failure is returned, recorded for `node_error` and not cached; the next request retries.

## Refresh flow

1. `refresh(scope)` returns a `RefreshHandle` at once. The job lists the keys in scope that are already cached (`Node` always runs its key), so a refresh stays lazy.
2. Events: `Started { total }`, then `Progress { done, total }` after each call and `NodeFailed { key, error }` for a failing node, then `Finished` as the last event: `Applied { refreshed, failed }`, `Cancelled` or `Closed`.
3. Between calls the worker serves queued loads and checks the `CancelFlag` (`cancel()` or `cancel_flag()`). A call already running finishes and its result is dropped.
4. Results are staged. At the end the worker applies them to a copy of the store outside the lock and swaps it in (`publish`): successes replace the old values and clear their errors, values equal to the cached ones keep their `Arc` and index, failures keep the old data, and objects that disappeared from a re-fetched listing are evicted with everything below them. Until the swap readers see the previous snapshot. A cancel leaves the store unchanged.
5. Dropping the handle lets the refresh finish.

## Snapshot and name index

1. The store is a copy-on-write `Arc<Store>` behind a `RwLock` that is held only to clone or swap the pointer. The worker is the only writer. Its maps are `ShardedMap`s (64 shards), so an apply copies only the shards it touches.
2. `snapshot()` is one read lock plus an `Arc` clone. It is synchronous, never waits on the worker or a refresh, and the result stays readable and unchanged after later refreshes. `generation()` grows with every apply and is only comparable between snapshots of the same `Catalog` (each starts at 0). Take a fresh snapshot per keystroke and do not store one: it keeps its data alive.
3. `listing`, `table_state`, `find_relations` (case-insensitive exact name among tables, views and materialized views in a scope) and `foreign_keys` read the snapshot. `table_state` separates `NotLoaded` from `Loaded` with zero columns and from `Failed`.
4. Every cached listing and table has a `NameIndex` (sorted lowercase names) built by the worker before publishing. `lookup(&NameQuery)` takes a text, a scope path, kinds (empty means all, columns included), a mode and a limit (default 50). Prefix mode is a binary search; fuzzy mode is a bounded subsequence scan. Hits are ranked Exact, Prefix, Substring, Fuzzy, then by penalty, name length and name, and cut to the limit before they allocate. Columns come only from loaded tables.
5. `foreign_keys(&obj)` is `None` until the table is loaded. The target keeps the source parent, with the schema replaced by `referenced_schema` when it is set.

## Loading columns on demand

`load_tables(Vec<DbObject>)` returns a `Send + 'static` future. Cache hits answer at once; each miss sends a `Load` when the method is called, so duplicates and a concurrent `table()` share one introspection call (the ADR-0014 single flight). Results come in input order. Dropping the future cancels the loads that have not started: the worker skips a `Load` whose reply is closed.

## Usage from the app

The crate spawns nothing. Spawn the worker on the tokio runtime, never on the GPUI thread (ADR-0007):

```rust,ignore
let (catalog, worker) = Catalog::new(session);
let task = handle.spawn(worker.run());           // tokio Handle

// Any thread: synchronous cache reads while drawing.
let rows = catalog.cached(&key);

// Completion: a snapshot per keystroke, no await.
let snap = catalog.snapshot();
let hits = snap.lookup(&NameQuery::new("ord").with_scope(schema_path));

// GPUI side: run the future on the runtime and await a oneshot with cx.spawn (SessionBridge).
let fut = catalog.children(key);                  // Send + 'static
handle.spawn(async move { tx.send(fut.await) });

// Close: take the session back and close it.
catalog.shutdown();
let source = task.await?;
```

## Limits

- One session serves loads and refreshes, so a cold load waits at most one introspection call of a running refresh, and a cancel takes effect after the running call.
- Paths are not normalised (ADR-0014): the scope `in_schema("main")` does not match objects listed under `None`.
- The index covers cached listings and loaded tables only; an object or a column that is not cached cannot be found. Fuzzy lookup is linear in the scoped names.
- Foreign keys are forward references of loaded tables. Reverse references and a dependency graph are not implemented.
- Every apply clones up to three shard vectors, so applying one table at a time over thousands of tables costs more than one batched refresh.
- DDL text (`definition`) is not cached.
