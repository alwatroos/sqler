// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The shared cache: lists, table details and per-node errors.
//!
//! The store is copy-on-write. [`Shared`] holds an `Arc<Store>`; readers clone that `Arc` under a
//! short read lock and keep a consistent snapshot, and the worker (the only writer) builds the
//! next store outside the lock and swaps it in. The lock is held only for a pointer clone or
//! swap and never across an `.await`.

use std::collections::HashSet;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, ObjectPath, Table};

use crate::error::CatalogError;
use crate::index::NameIndex;
use crate::key::{CacheKey, NodeKey, path_depth, path_within};
use crate::shard::ShardedMap;

/// The store as shared between the worker and the catalog handles.
pub(crate) type Shared = Arc<RwLock<Arc<Store>>>;

/// Creates an empty shared store.
pub(crate) fn new_shared() -> Shared {
    Arc::new(RwLock::new(Arc::new(Store::default())))
}

/// Locks for reading; a poisoned lock is recovered.
pub(crate) fn read(shared: &RwLock<Arc<Store>>) -> RwLockReadGuard<'_, Arc<Store>> {
    shared.read().unwrap_or_else(PoisonError::into_inner)
}

/// Locks for writing; a poisoned lock is recovered.
pub(crate) fn write(shared: &RwLock<Arc<Store>>) -> RwLockWriteGuard<'_, Arc<Store>> {
    shared.write().unwrap_or_else(PoisonError::into_inner)
}

/// The current store: one read lock and one `Arc` clone.
pub(crate) fn current(shared: &RwLock<Arc<Store>>) -> Arc<Store> {
    Arc::clone(&read(shared))
}

/// A cached listing with the index over its names.
#[derive(Debug)]
pub(crate) struct ListEntry {
    pub(crate) listing: Arc<Listing>,
    /// Names of the listed objects; a slot is the position in the listing.
    pub(crate) names: NameIndex,
}

impl ListEntry {
    /// Wraps `listing` and indexes its names.
    pub(crate) fn new(listing: Arc<Listing>) -> Self {
        let names = match &*listing {
            Listing::Objects(objects) => NameIndex::build(objects.iter().map(|o| o.name.as_str())),
            Listing::Unsupported => NameIndex::default(),
        };
        Self { listing, names }
    }
}

/// Cached table details with the index over its column names.
#[derive(Debug)]
pub(crate) struct TableEntry {
    pub(crate) table: Arc<Table>,
    /// Column names; a slot is the position in `table.columns`.
    pub(crate) columns: NameIndex,
}

impl TableEntry {
    /// Wraps `table` and indexes its column names.
    pub(crate) fn new(table: Arc<Table>) -> Self {
        let columns = NameIndex::build(table.columns.iter().map(|c| c.name.as_str()));
        Self { table, columns }
    }
}

/// Results waiting to be applied in one step.
#[derive(Debug, Default)]
pub(crate) struct Staged {
    pub(crate) lists: Vec<(NodeKey, Arc<ListEntry>)>,
    pub(crate) tables: Vec<(DbObject, Arc<TableEntry>)>,
    /// Tables a refresh found to be gone.
    pub(crate) gone: Vec<DbObject>,
    pub(crate) failures: Vec<(CacheKey, CatalogError)>,
    /// Entries re-fetched with an equal value: their data stays as it is, only the error clears.
    pub(crate) unchanged: Vec<CacheKey>,
}

/// The cached data. Cloning shares everything; a mutation copies only the touched shards.
#[derive(Debug, Clone, Default)]
pub(crate) struct Store {
    pub(crate) lists: ShardedMap<NodeKey, Arc<ListEntry>>,
    pub(crate) tables: ShardedMap<DbObject, Arc<TableEntry>>,
    pub(crate) errors: ShardedMap<CacheKey, CatalogError>,
    /// Grows by one with every change, so equal generations mean an equal store.
    pub(crate) generation: u64,
}

/// Kinds whose objects contain other cached nodes.
fn is_container(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Database
            | ObjectKind::Schema
            | ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
    )
}

impl Store {
    /// Number of cached lists plus tables.
    pub(crate) fn len(&self) -> usize {
        self.lists.len() + self.tables.len()
    }

    /// Whether `key` has cached data.
    #[cfg(test)]
    pub(crate) fn contains(&self, key: &CacheKey) -> bool {
        match key {
            CacheKey::Children(k) => self.lists.contains_key(k),
            CacheKey::Table(o) => self.tables.contains_key(o),
        }
    }

    /// Heap bytes of every name index.
    pub(crate) fn index_bytes(&self) -> usize {
        let lists: usize = self.lists.iter().map(|(_, e)| e.names.heap_bytes()).sum();
        let tables: usize = self
            .tables
            .iter()
            .map(|(_, e)| e.columns.heap_bytes())
            .sum();
        lists + tables
    }

    /// The cached keys within `path`, parents before children.
    pub(crate) fn keys_within(&self, path: &ObjectPath) -> Vec<CacheKey> {
        let lists = self.lists.keys().map(|k| CacheKey::Children(k.clone()));
        let tables = self.tables.keys().map(|o| CacheKey::Table(o.clone()));
        let mut keys: Vec<CacheKey> = lists.chain(tables).filter(|k| k.is_within(path)).collect();
        keys.sort_by_cached_key(sort_key);
        keys
    }

    /// Removes the error of `key`, if any.
    pub(crate) fn clear_error(&mut self, key: &CacheKey) {
        self.errors.remove(key);
        self.generation += 1;
    }

    /// Applies staged results.
    ///
    /// Successes replace data and clear errors. Failures are recorded and remove nothing.
    /// Objects of a container kind that a re-fetched listing no longer contains are evicted
    /// together with everything under them.
    pub(crate) fn apply(&mut self, staged: Staged) {
        self.generation += 1;
        let mut evict: Vec<ObjectPath> = Vec::new();
        for (key, entry) in staged.lists {
            self.errors.remove(&CacheKey::Children(key.clone()));
            if let Some(old) = self.lists.insert(key, Arc::clone(&entry)) {
                evict.extend(dropped_paths(&old.listing, &entry.listing));
            }
        }
        for (obj, entry) in staged.tables {
            self.errors.remove(&CacheKey::Table(obj.clone()));
            self.tables.insert(obj, entry);
        }
        for key in &staged.unchanged {
            self.errors.remove(key);
        }
        for obj in staged.gone {
            self.errors.remove(&CacheKey::Table(obj.clone()));
            self.tables.remove(&obj);
        }
        for (key, error) in staged.failures {
            self.errors.insert(key, error);
        }
        for path in &evict {
            self.lists.retain(|k, _| !path_within(&k.parent, path));
            self.tables
                .retain(|o, _| !path_within(&o.child_path(), path));
            self.errors.retain(|k, _| !k.is_within(path));
        }
    }
}

/// The child paths of container objects in `old` that `new` no longer lists.
fn dropped_paths(old: &Listing, new: &Listing) -> Vec<ObjectPath> {
    let (Listing::Objects(old), Listing::Objects(new)) = (old, new) else {
        return Vec::new();
    };
    let kept: HashSet<&DbObject> = new.iter().collect();
    old.iter()
        .filter(|o| is_container(o.kind) && !kept.contains(o))
        .map(DbObject::child_path)
        .collect()
}

type SortKey = (
    usize,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    ObjectKind,
);

fn sort_key(key: &CacheKey) -> SortKey {
    let path = key.path();
    let depth = path_depth(&path);
    let (is_table, kind) = match key {
        CacheKey::Children(k) => (false, k.kind),
        CacheKey::Table(o) => (true, o.kind),
    };
    (
        depth,
        path.database,
        path.schema,
        path.table,
        is_table,
        kind,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main() -> ObjectPath {
        ObjectPath::root().in_schema("main")
    }

    fn tbl(name: &str) -> DbObject {
        DbObject::new(ObjectKind::Table, main(), name)
    }

    fn list(objs: &[&DbObject]) -> Arc<ListEntry> {
        Arc::new(ListEntry::new(Arc::new(Listing::Objects(
            objs.iter().map(|o| (*o).clone()).collect(),
        ))))
    }

    fn entry(obj: &DbObject) -> Arc<TableEntry> {
        Arc::new(TableEntry::new(Arc::new(Table::new(obj.clone()))))
    }

    fn tables_key() -> NodeKey {
        NodeKey::new(main(), ObjectKind::Table)
    }

    fn index_key(name: &str) -> NodeKey {
        NodeKey::new(main().in_table(name), ObjectKind::Index)
    }

    fn seeded() -> Store {
        let (a, b) = (tbl("a"), tbl("b"));
        let mut s = Store::default();
        s.apply(Staged {
            lists: vec![
                (tables_key(), list(&[&a, &b])),
                (index_key("a"), list(&[])),
                (index_key("b"), list(&[])),
            ],
            tables: vec![(a.clone(), entry(&a)), (b.clone(), entry(&b))],
            ..Staged::default()
        });
        s
    }

    #[test]
    fn apply_inserts_and_counts() {
        let s = seeded();
        assert_eq!(s.len(), 5);
        assert!(s.contains(&CacheKey::Table(tbl("a"))));
        assert!(!s.contains(&CacheKey::Table(tbl("z"))));
    }

    #[test]
    fn keys_are_sorted_by_depth() {
        let s = seeded();
        let keys = s.keys_within(&ObjectPath::root());
        assert_eq!(keys.len(), 5);
        let depths: Vec<_> = keys.iter().map(CacheKey::depth).collect();
        assert!(depths.windows(2).all(|w| w[0] <= w[1]), "{depths:?}");
        assert_eq!(keys[0], CacheKey::Children(tables_key()));
        assert_eq!(s.keys_within(&main().in_table("a")).len(), 2);
        assert_eq!(s.keys_within(&ObjectPath::root().in_schema("x")).len(), 0);
    }

    #[test]
    fn failure_keeps_data_and_success_clears_it() {
        let mut s = seeded();
        let key = CacheKey::Children(tables_key());
        s.apply(Staged {
            failures: vec![(key.clone(), CatalogError::Driver("boom".into()))],
            ..Staged::default()
        });
        assert_eq!(
            s.errors.get(&key),
            Some(&CatalogError::Driver("boom".into()))
        );
        assert_eq!(s.len(), 5);
        s.apply(Staged {
            lists: vec![(tables_key(), list(&[&tbl("a"), &tbl("b")]))],
            ..Staged::default()
        });
        assert!(s.errors.is_empty());
    }

    #[test]
    fn dropped_table_is_pruned_with_its_details() {
        let mut s = seeded();
        let a = tbl("a");
        s.errors.insert(
            CacheKey::Children(index_key("b")),
            CatalogError::Driver("old".into()),
        );
        s.apply(Staged {
            lists: vec![(tables_key(), list(&[&a]))],
            ..Staged::default()
        });
        assert!(s.contains(&CacheKey::Table(a)));
        assert!(!s.contains(&CacheKey::Table(tbl("b"))));
        assert!(!s.contains(&CacheKey::Children(index_key("b"))));
        assert!(s.contains(&CacheKey::Children(index_key("a"))));
        assert!(s.errors.is_empty());
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn dropped_schema_evicts_its_subtree_and_non_containers_are_ignored() {
        let mut s = Store::default();
        let schema = |n: &str| DbObject::new(ObjectKind::Schema, ObjectPath::root(), n);
        let schemas = NodeKey::new(ObjectPath::root(), ObjectKind::Schema);
        let other = ObjectPath::root().in_schema("other");
        let t = DbObject::new(ObjectKind::Table, other.clone(), "t");
        let f = DbObject::new(ObjectKind::Function, other.clone(), "f");
        s.apply(Staged {
            lists: vec![
                (schemas.clone(), list(&[&schema("main"), &schema("other")])),
                (NodeKey::new(other.clone(), ObjectKind::Table), list(&[&t])),
                (
                    NodeKey::new(other.clone(), ObjectKind::Function),
                    list(&[&f]),
                ),
                (NodeKey::new(main(), ObjectKind::Table), list(&[])),
            ],
            ..Staged::default()
        });
        // A dropped function has no children and evicts nothing.
        s.apply(Staged {
            lists: vec![(NodeKey::new(other, ObjectKind::Function), list(&[]))],
            ..Staged::default()
        });
        assert_eq!(s.len(), 4);
        s.apply(Staged {
            lists: vec![(schemas, list(&[&schema("main")]))],
            ..Staged::default()
        });
        assert_eq!(s.len(), 2);
        assert!(s.contains(&CacheKey::Children(NodeKey::new(main(), ObjectKind::Table))));
    }

    #[test]
    fn unsupported_listings_never_prune() {
        let mut s = seeded();
        s.apply(Staged {
            lists: vec![(
                tables_key(),
                Arc::new(ListEntry::new(Arc::new(Listing::Unsupported))),
            )],
            ..Staged::default()
        });
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn gone_tables_are_removed() {
        let mut s = seeded();
        s.apply(Staged {
            gone: vec![tbl("a")],
            ..Staged::default()
        });
        assert!(!s.contains(&CacheKey::Table(tbl("a"))));
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn unchanged_clears_the_error_and_keeps_the_entry() {
        let mut s = seeded();
        let key = CacheKey::Children(tables_key());
        s.apply(Staged {
            failures: vec![(key.clone(), CatalogError::Driver("boom".into()))],
            ..Staged::default()
        });
        let kept = Arc::clone(s.lists.get(&tables_key()).unwrap());
        s.apply(Staged {
            unchanged: vec![key],
            ..Staged::default()
        });
        assert!(s.errors.is_empty());
        assert!(Arc::ptr_eq(&kept, s.lists.get(&tables_key()).unwrap()));
    }

    #[test]
    fn generation_grows_and_clones_are_independent() {
        let mut s = seeded();
        let g = s.generation;
        let before = s.clone();
        s.clear_error(&CacheKey::Children(tables_key()));
        assert_eq!(s.generation, g + 1);
        s.apply(Staged {
            gone: vec![tbl("a")],
            ..Staged::default()
        });
        assert_eq!(s.generation, g + 2);
        assert_eq!(before.generation, g);
        assert_eq!(before.len(), 5);
        assert_eq!(s.len(), 4);
        // Entries the apply did not touch are shared with the older store.
        let (old, new) = (
            before.lists.get(&tables_key()).unwrap(),
            s.lists.get(&tables_key()).unwrap(),
        );
        assert!(Arc::ptr_eq(old, new));
    }

    #[test]
    fn entries_index_their_names() {
        let list = list(&[&tbl("Users"), &tbl("orders")]);
        assert_eq!(list.names.exact("users").count(), 1);
        let unsupported = ListEntry::new(Arc::new(Listing::Unsupported));
        assert_eq!(unsupported.names.heap_bytes(), 0);
        let t =
            Table::new(tbl("t")).with_columns(vec![sqler_driver_api::sqler_core::ColumnInfo::new(
                "Id",
                sqler_driver_api::sqler_core::DataType::Integer,
            )]);
        let entry = TableEntry::new(Arc::new(t));
        assert_eq!(entry.columns.exact("id").count(), 1);
    }

    #[test]
    fn poisoned_lock_is_recovered() {
        let shared: Shared = Arc::new(RwLock::new(Arc::new(seeded())));
        let clone = Arc::clone(&shared);
        let _ = std::thread::spawn(move || {
            let _guard = clone.write().unwrap();
            panic!("poison");
        })
        .join();
        assert!(shared.is_poisoned());
        assert_eq!(read(&shared).len(), 5);
        Arc::make_mut(&mut write(&shared)).clear_error(&CacheKey::Children(tables_key()));
        assert_eq!(current(&shared).len(), 5);
    }
}
