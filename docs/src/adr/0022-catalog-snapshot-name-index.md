# 0022 Catalog snapshot, name index and on-demand column loading

- Status: accepted
- Date: 2026-10-20

## Context and problem statement

Completion and hover (T6, T7, T10) read the catalog on every keystroke, on a thread that must not stall. ADR-0014 gives cached reads and a background refresh, but its store is a `RwLock<Store>` of three plain maps: a reader sees no consistent view across several reads, an apply holds the write lock for thousands of inserts, and there is no name search. Intellisense needs a consistent immutable view readable from any thread, a bounded and ranked name lookup over objects and columns on a 5,000-table schema, the columns of the tables a query names, and the foreign-key targets of loaded tables (hover). A full dependency graph is out of scope.

## Decision drivers

- Taking a snapshot must not wait for the worker or a refresh.
- A refresh publishes incrementally: objects that did not change are shared with the old snapshot.
- Lookups on 5,000 tables must stay far below a frame.
- "Not loaded" must differ from "no columns".
- No new dependency; the ADR-0014 API keeps working.

## Considered options

1. Copy-on-write store behind `Arc<RwLock<Arc<Store>>>`, sharded maps, a sorted name index per listing and table.
2. A persistent map crate (`im`, `rpds`, `imbl`).
3. `arc-swap` for the published pointer.
4. Search the live store under the read lock, without an index.

## Decision outcome

Chosen: option 1.

**Copy-on-write snapshot.** `Shared` changes from `Arc<RwLock<Store>>` to `Arc<RwLock<Arc<Store>>>`. The worker is the only writer. `publish` clones the current `Arc<Store>` (a few `Arc` clones) under a read guard, drops the guard, applies its staged results to the clone and swaps the new `Arc<Store>` in with a write lock held for one pointer assignment. `Catalog::snapshot()` is one read lock plus an `Arc` clone, synchronous, and never waits on the worker. A `CatalogSnapshot` is the `Arc<Store>` itself: it is immutable, `Send + Sync`, and carries a `generation` counter that grows with every apply and `clear_error`. No lock is held across `.await`.

**Sharded maps.** `ShardedMap<K, V>` is 64 `Arc<HashMap>` shards (shard = fixed-key `DefaultHasher` of the key, modulo 64). Mutators use `Arc::make_mut`, so an apply copies only the shards it touches; `retain` copies a shard only when it removes from it. The store holds `lists`, `tables` and `errors` as `ShardedMap`s. On refresh the worker compares the fetched `Listing` or `Table` with the cached one; when they are equal the old `Arc` and its index are kept and the key goes to `unchanged` (only its error is cleared). Pruning (ADR-0014) is unchanged and shows up in the next snapshot.

**Name index.** Every cached listing and table carries a `NameIndex`: its names lowercased with `str::to_lowercase` (so `ÉTÉ` matches `été`) and sorted, each with its slot in the source slice. The worker builds the `ListEntry` or `TableEntry` before publishing, outside any lock. `CatalogSnapshot::lookup(&NameQuery)` walks the listings and loaded tables whose path lies within the query scope, filters by kind (objects by their `ObjectKind`, columns by `NameKind::Column`) and matches:

- Prefix mode: a binary search (`partition_point`) over the sorted names. It yields `Exact` and `Prefix` hits. Empty text matches everything as `Prefix`.
- Fuzzy mode: a bounded linear scan for a case-insensitive subsequence. It yields `Exact`, `Prefix`, `Substring` (penalty = start offset) and `Fuzzy` (penalty = start offset plus gaps).

Candidates are light: they borrow the cached names; only names that need case folding (uppercase or non-ASCII) allocate a folded copy for ranking. They are cut to `limit` with `select_nth_unstable`, sorted by (quality, penalty, name length in chars, folded name, kind, parent, then the original-case name and the object path as the final tie-break, so the order is total), and only then turned into `NameHit`s. The default limit is 50. Paths are not normalised (ADR-0014): the scope `in_schema("main")` does not match objects listed under `None`.

**Columns on demand.** `Catalog::load_tables(Vec<DbObject>)` answers cache hits at once and sends a `Load` per miss at call time, so it reuses the single-flight path of ADR-0014: duplicates and a concurrent `table()` cost one introspection call per table. The future is `Send + 'static`; dropping it closes the reply channels, and the worker skips a `Load` whose reply is closed before it fetches, so loads that have not started cost nothing. `CatalogSnapshot::table_state` reports `NotLoaded`, `Loaded(Arc<Table>)` (possibly with zero columns) or `Failed(CatalogError)`.

**Foreign keys.** `foreign_keys(&DbObject)` returns `None` unless the table is loaded, otherwise one `ForeignKeyTarget` per constraint. The target is a table in the same parent as the source, with its `schema` replaced by `referenced_schema` when that is set. Only forward references of loaded tables are covered; reverse references and a dependency graph are not.

**Memory.** An index costs the lowercased names plus the fixed size of one entry (a boxed string and a slot). `index_bytes()` reports the total; the bench budget is 4 MiB for the 5,000-table catalog. An old snapshot keeps its data alive until it is dropped, so callers take a fresh snapshot per keystroke and do not store one.

## Consequences

- Good: a snapshot is a read that costs one read lock and an `Arc` clone and a consistent view; refresh never blocks it.
- Good: an apply holds the write lock for a pointer swap, and unchanged objects and indexes are shared across generations.
- Good: prefix lookup is logarithmic; fuzzy lookup is bounded and ranked; results are limited before they allocate.
- Good: no new dependency, no new licence.
- Bad: every apply clones up to three shard vectors (64 pointers each) plus the touched shards. A sequential `table()` per table over 5,000 tables costs about N²/64 key clones. Measured with `catalog_tree` (fake source, 5,000 tables, 5,002 nodes, macOS arm64, three runs each, before this change on the base commit and after it): `refresh_all_ms` 4.56, 4.48, 4.49 before and 4.35, 4.40, 4.31 after, so the single batched apply of a refresh did not get slower (equal values are reused and only changed shards are copied). The sequential loading loop is setup in that bench and is not timed. Other `catalog_tree` figures: cold expand of 5,000 tables p95 138 to 322 us before and 207 to 526 us after (the 5,000-name index is built on that path), refresh of the schema p95 about 460 us before and 229 us after, cached read p95 42 ns before and 84 ns after (one timer tick of about 41 ns; the read now takes a lock, an `Arc` clone and a shard lookup), still far under its 10 us budget and inside the 25 % + 50 ns `--compare` slack. Index numbers are in [benchmarks](../benchmarks.md#catalog-index).
- Bad: the indexes add memory per cached listing and table, and an old snapshot pins its generation.
- Bad: fuzzy lookup is linear in the scoped names.
- Neutral: `lookup` finds columns only of loaded tables; `NotLoaded` tables contribute none.

## Pros and cons of the options

- Copy-on-write store with sharded maps and a sorted index: chosen, see above.
- `im`, `rpds` or `imbl`: persistent maps would avoid the shard copies, but add a dependency; `imbl` and `im` are MPL-2.0, which `deny.toml` allows only for listed crates. Rejected.
- `arc-swap`: lock-free swaps, but a read lock held only for an `Arc` clone and a write lock held only for a pointer swap already cost about the same, so it adds a dependency for nothing. Rejected.
- Search under the read lock without an index: every keystroke would scan and lowercase 5,000 names, and a long scan would block the worker's apply. Rejected.
