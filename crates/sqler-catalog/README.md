# sqler-catalog

Per-connection schema cache for the object browser. It loads tree nodes on demand through `Introspect` (ADR-0008), refreshes them in the background and never blocks a reader.

Status: implemented (M2, T7); snapshot, name index and on-demand column loading added in M3 (T5, ADR-0022). The dependency graph is not implemented.

Layer: L2. Depends on `sqler-core` (through `sqler-driver-api`), `sqler-driver-api`, `thiserror` and `tokio` (`sync` only). The crate spawns nothing.

## API

| Type | Role |
| ---- | ---- |
| `Catalog` | cloneable handle (`Send + Sync`); `new(source)` returns the handle and a `CatalogWorker` |
| `CatalogWorker` | `run().await` owns the session, runs every introspection call in sequence and returns the `IntrospectSource` on shutdown |
| `IntrospectSource` | `Session` or `Introspect` box; `From` for both |
| `NodeKey`, `CacheKey`, `RefreshScope` | what is cached (`Children(NodeKey)` or `Table(DbObject)`) and what to refresh (`Node`, `Subtree`, `All`) |
| `RefreshHandle`, `RefreshEvent`, `RefreshOutcome` | progress channel and cancel of one refresh |
| `CatalogSnapshot`, `TableState`, `ForeignKeyTarget` | immutable view of the cache; `NotLoaded` / `Loaded` / `Failed` per table; forward foreign-key targets |
| `NameQuery`, `NameHit`, `NameKind`, `MatchMode`, `MatchQuality` | scoped, bounded, ranked name lookup |
| `CatalogError` | `Driver`, `Cancelled`, `Closed`, `NoIntrospection` |

Reads: `cached`, `cached_table`, `node_error` and `cached_len` are synchronous and never touch the database. `children` and `table` return a ready result on a cache hit and ask the worker on a miss. Concurrent misses on the same node cost one round-trip.

Snapshot and index: `snapshot()` is one read lock and an `Arc` clone and never waits for the worker or a refresh. `CatalogSnapshot::lookup` does prefix (binary search) or fuzzy (subsequence) matching over cached listings and the columns of loaded tables, scoped by path, bounded (default 50) and ranked Exact, Prefix, Substring, Fuzzy. `table_state` tells not loaded from no columns, `foreign_keys` resolves forward references of loaded tables, and `load_tables` loads the columns of named tables on demand with one call per table. Take a fresh snapshot per request: it keeps its data alive. Decision: [ADR-0022](../../docs/src/adr/0022-catalog-snapshot-name-index.md).

## Usage

```rust,ignore
let (catalog, worker) = Catalog::new(session);       // session: Box<dyn Session>
let task = handle.spawn(worker.run());               // tokio Handle, never the GPUI thread

let key = NodeKey::new(ObjectPath::root(), ObjectKind::Schema);
let schemas = catalog.children(key).await?;           // one list() call, then cached

let mut refresh = catalog.refresh(RefreshScope::All);
while let Some(event) = refresh.next_event().await {  // Started, Progress, NodeFailed, Finished
    // update the UI; refresh.cancel() stops it
}

catalog.shutdown();
let source = task.await?;                             // take the session back and close it
```

A refresh applies its results in one step at the end (a copy-on-write swap), so readers keep the previous snapshot until then. Design and policy: [ADR-0014](../../docs/src/adr/0014-catalog-cache-and-background-refresh.md) and [the catalog page](../../docs/src/architecture/catalog.md).

## Tests and benchmark

- `cargo nextest run -p sqler-catalog` (fake `Introspect` and the `SQLite` driver, no Docker).
- `cargo bench -p sqler-catalog --bench catalog_tree` and `--bench catalog_index` (5,000 tables; results in `docs/src/benchmarks.md`).

See [AGENTS.md](../../AGENTS.md) for the project handbook.
