// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! An immutable view of the cached catalog: lookups, table state and foreign-key targets.

use std::borrow::Cow;
use std::sync::Arc;

use sqler_driver_api::sqler_core::{DbObject, Listing, ObjectKind, ObjectPath, Table};

use crate::error::CatalogError;
use crate::index::{MatchQuality, NameHit, NameKind, NameQuery, Ranked, fold, path_key, rank};
use crate::key::{CacheKey, NodeKey, path_within};
use crate::store::Store;

/// What the snapshot knows about the details (columns, keys) of one table.
///
/// `NotLoaded` is different from `Loaded` with zero columns: the first says nobody asked yet.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TableState {
    /// The details were never loaded (or were evicted).
    NotLoaded,
    /// The details are cached.
    Loaded(Arc<Table>),
    /// The last load of the details failed and nothing is cached.
    Failed(CatalogError),
}

/// Where one foreign key of a loaded table points.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForeignKeyTarget {
    /// The constraint name, if the database reports one.
    pub name: Option<String>,
    /// The referencing columns of the loaded table.
    pub columns: Vec<String>,
    /// The referenced table. Its schema is the key's `referenced_schema` when set, else the
    /// schema of the referencing table.
    pub target: DbObject,
    /// The referenced columns, in the order of `columns`.
    pub referenced_columns: Vec<String>,
}

/// An immutable, consistent view of the cache at one moment.
///
/// Taking one is a single read lock and an `Arc` clone ([`crate::Catalog::snapshot`]); reading it
/// never waits for the worker. It keeps the data it shows alive, so take a fresh snapshot per
/// request and do not store one for long.
#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    store: Arc<Store>,
}

/// A candidate hit before the strings of the winners are copied.
struct Candidate<'a> {
    quality: MatchQuality,
    penalty: u32,
    name: &'a str,
    name_chars: usize,
    folded: Cow<'a, str>,
    kind: NameKind,
    parent: Parent<'a>,
}

enum Parent<'a> {
    Listed(&'a ObjectPath),
    Table(Arc<ObjectPath>),
}

impl Parent<'_> {
    fn get(&self) -> &ObjectPath {
        match self {
            Parent::Listed(p) => p,
            Parent::Table(p) => p,
        }
    }
}

impl Candidate<'_> {
    fn ranked(&self) -> Ranked<'_> {
        Ranked {
            quality: self.quality,
            penalty: self.penalty,
            name_chars: self.name_chars,
            folded: &self.folded,
            kind: self.kind,
            parent: self.parent.get(),
        }
    }
}

/// The folded form of `name`, borrowed when folding would not change it.
fn folded(name: &str) -> Cow<'_, str> {
    if name.is_ascii() && !name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(fold(name))
    }
}

fn is_relation(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
    )
}

impl CatalogSnapshot {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Grows with every change of the cache. Equal generations of snapshots of the same `Catalog`
    /// mean equal content. Generations are only comparable within one `Catalog`: each starts at 0.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.store.generation
    }

    /// The cached children of `key`, if loaded.
    #[must_use]
    pub fn listing(&self, key: &NodeKey) -> Option<Arc<Listing>> {
        self.store.lists.get(key).map(|e| Arc::clone(&e.listing))
    }

    /// The state of the details of `obj`.
    #[must_use]
    pub fn table_state(&self, obj: &DbObject) -> TableState {
        if let Some(entry) = self.store.tables.get(obj) {
            return TableState::Loaded(Arc::clone(&entry.table));
        }
        match self.store.errors.get(&CacheKey::Table(obj.clone())) {
            Some(error) => TableState::Failed(error.clone()),
            None => TableState::NotLoaded,
        }
    }

    /// Names matching `query`, best first, at most `query.limit`.
    ///
    /// Covers every cached listing whose parent path is within `query.scope` and the columns of
    /// every loaded table within it. Ranked by match quality, penalty, name length, folded name,
    /// kind and parent, so the order is stable.
    #[must_use]
    pub fn lookup(&self, query: &NameQuery) -> Vec<NameHit> {
        if query.limit == 0 {
            return Vec::new();
        }
        let needle = fold(&query.text);
        let wants = |kind: NameKind| query.kinds.is_empty() || query.kinds.contains(&kind);
        let mut found: Vec<Candidate<'_>> = Vec::new();

        for (key, entry) in self.store.lists.iter() {
            if !path_within(&key.parent, &query.scope) {
                continue;
            }
            let Listing::Objects(objects) = &*entry.listing else {
                continue;
            };
            entry
                .names
                .search(&needle, query.mode, &mut |slot, quality, penalty| {
                    let Some(obj) = usize::try_from(slot).ok().and_then(|i| objects.get(i)) else {
                        return;
                    };
                    let kind = NameKind::Object(obj.kind);
                    if wants(kind) {
                        found.push(Candidate {
                            quality,
                            penalty,
                            name: &obj.name,
                            name_chars: obj.name.chars().count(),
                            folded: folded(&obj.name),
                            kind,
                            parent: Parent::Listed(&key.parent),
                        });
                    }
                });
        }

        if wants(NameKind::Column) {
            for (obj, entry) in self.store.tables.iter() {
                let columns = &entry.table.columns;
                if columns.is_empty() {
                    continue;
                }
                let path = obj.child_path();
                if !path_within(&path, &query.scope) {
                    continue;
                }
                let path = Arc::new(path);
                entry
                    .columns
                    .search(&needle, query.mode, &mut |slot, quality, penalty| {
                        let Some(col) = usize::try_from(slot).ok().and_then(|i| columns.get(i))
                        else {
                            return;
                        };
                        found.push(Candidate {
                            quality,
                            penalty,
                            name: &col.name,
                            name_chars: col.name.chars().count(),
                            folded: folded(&col.name),
                            kind: NameKind::Column,
                            parent: Parent::Table(Arc::clone(&path)),
                        });
                    });
            }
        }

        let order = |a: &Candidate<'_>, b: &Candidate<'_>| {
            rank(&a.ranked(), &b.ranked())
                .then_with(|| a.name.cmp(b.name))
                .then_with(|| path_key(a.parent.get()).cmp(&path_key(b.parent.get())))
        };
        if found.len() > query.limit {
            found.select_nth_unstable_by(query.limit - 1, order);
            found.truncate(query.limit);
        }
        found.sort_by(order);
        found
            .into_iter()
            .map(|c| NameHit {
                name: c.name.to_owned(),
                kind: c.kind,
                parent: c.parent.get().clone(),
                quality: c.quality,
            })
            .collect()
    }

    /// The tables, views and materialised views named `name` (ignoring case) whose parent path
    /// is within `scope`, in a stable order.
    #[must_use]
    pub fn find_relations(&self, scope: &ObjectPath, name: &str) -> Vec<DbObject> {
        let lower = fold(name);
        let mut out = Vec::new();
        for (key, entry) in self.store.lists.iter() {
            if !path_within(&key.parent, scope) {
                continue;
            }
            let Listing::Objects(objects) = &*entry.listing else {
                continue;
            };
            for slot in entry.names.exact(&lower) {
                let obj = usize::try_from(slot).ok().and_then(|i| objects.get(i));
                if let Some(obj) = obj.filter(|o| is_relation(o.kind)) {
                    out.push(obj.clone());
                }
            }
        }
        out.sort_by(|a, b| {
            let key = |o: &DbObject| {
                (
                    o.parent.database.clone(),
                    o.parent.schema.clone(),
                    o.parent.table.clone(),
                    o.kind,
                    o.name.clone(),
                )
            };
            key(a).cmp(&key(b))
        });
        out.dedup();
        out
    }

    /// Where the foreign keys of `obj` point; `None` unless its details are loaded.
    #[must_use]
    pub fn foreign_keys(&self, obj: &DbObject) -> Option<Vec<ForeignKeyTarget>> {
        let entry = self.store.tables.get(obj)?;
        Some(
            entry
                .table
                .foreign_keys
                .iter()
                .map(|fk| {
                    let mut parent = obj.parent.clone();
                    if let Some(schema) = &fk.referenced_schema {
                        parent.schema = Some(schema.clone());
                    }
                    ForeignKeyTarget {
                        name: fk.name.clone(),
                        columns: fk.columns.clone(),
                        target: DbObject::new(
                            ObjectKind::Table,
                            parent,
                            fk.referenced_table.clone(),
                        ),
                        referenced_columns: fk.referenced_columns.clone(),
                    }
                })
                .collect(),
        )
    }

    /// Heap bytes held by the name indexes of this snapshot (not the listings or tables).
    #[must_use]
    pub fn index_bytes(&self) -> usize {
        self.store.index_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::MatchMode;
    use crate::store::{ListEntry, Staged, TableEntry};
    use sqler_driver_api::sqler_core::{ColumnInfo, DataType, ForeignKey};

    fn main() -> ObjectPath {
        ObjectPath::root().in_schema("main")
    }

    fn obj(kind: ObjectKind, name: &str) -> DbObject {
        DbObject::new(kind, main(), name)
    }

    fn listing(kind: ObjectKind, names: &[&str]) -> (NodeKey, Arc<ListEntry>) {
        let objects = names.iter().map(|n| obj(kind, n)).collect();
        (
            NodeKey::new(main(), kind),
            Arc::new(ListEntry::new(Arc::new(Listing::Objects(objects)))),
        )
    }

    fn table(name: &str, cols: &[&str]) -> (DbObject, Arc<TableEntry>) {
        let o = obj(ObjectKind::Table, name);
        let columns = cols
            .iter()
            .map(|c| ColumnInfo::new(*c, DataType::Integer))
            .collect();
        let t = Table::new(o.clone()).with_columns(columns);
        (o, Arc::new(TableEntry::new(Arc::new(t))))
    }

    fn snapshot() -> CatalogSnapshot {
        let mut store = Store::default();
        store.apply(Staged {
            lists: vec![
                listing(
                    ObjectKind::Table,
                    &["Users", "orders", "order_items", "tmp"],
                ),
                listing(ObjectKind::View, &["user_view"]),
            ],
            tables: vec![table("orders", &["id", "user_id", "Total"])],
            ..Staged::default()
        });
        CatalogSnapshot::new(Arc::new(store))
    }

    fn names(hits: &[NameHit]) -> Vec<&str> {
        hits.iter().map(|h| h.name.as_str()).collect()
    }

    #[test]
    fn prefix_lookup_ranks_exact_first_then_shorter() {
        let s = snapshot();
        let hits = s.lookup(&NameQuery::new("USER"));
        assert_eq!(names(&hits), ["Users", "user_id", "user_view"]);
        assert_eq!(hits[0].quality, MatchQuality::Prefix);
        let hits = s.lookup(&NameQuery::new("tmp"));
        assert_eq!(hits[0].quality, MatchQuality::Exact);
    }

    #[test]
    fn kinds_scope_and_limit_apply() {
        let s = snapshot();
        let only_cols = NameQuery::new("").with_kinds(vec![NameKind::Column]);
        assert_eq!(names(&s.lookup(&only_cols)), ["id", "Total", "user_id"]);
        let views = NameQuery::new("u").with_kinds(vec![NameKind::Object(ObjectKind::View)]);
        assert_eq!(names(&s.lookup(&views)), ["user_view"]);
        let elsewhere = NameQuery::new("").with_scope(ObjectPath::root().in_schema("other"));
        assert_eq!(s.lookup(&elsewhere).len(), 0);
        let in_table = NameQuery::new("").with_scope(main().in_table("orders"));
        assert_eq!(s.lookup(&in_table).len(), 3);
        assert_eq!(s.lookup(&NameQuery::new("").with_limit(2)).len(), 2);
        assert_eq!(s.lookup(&NameQuery::new("").with_limit(0)).len(), 0);
        let top = s.lookup(&NameQuery::new("").with_limit(1));
        let all = s.lookup(&NameQuery::new("").with_limit(100));
        assert_eq!(top[0], all[0]);
    }

    #[test]
    fn fuzzy_lookup_orders_by_quality() {
        let s = snapshot();
        let hits = s.lookup(&NameQuery::new("ord").fuzzy());
        assert_eq!(names(&hits)[..2], ["orders", "order_items"]);
        let hits = s.lookup(&NameQuery::new("oit").fuzzy());
        assert_eq!(names(&hits), ["order_items"]);
        assert_eq!(hits[0].quality, MatchQuality::Fuzzy);
        let hits = s.lookup(&NameQuery::new("ers").fuzzy());
        assert_eq!(hits[0].quality, MatchQuality::Substring);
        assert_eq!(
            s.lookup(&NameQuery::new("oit")).len(),
            0,
            "prefix mode never matches a subsequence"
        );
        assert_eq!(NameQuery::new("x").fuzzy().mode, MatchMode::Fuzzy);
    }

    #[test]
    fn hits_name_objects_and_columns() {
        let s = snapshot();
        let hit = &s.lookup(&NameQuery::new("order_i"))[0];
        assert_eq!(hit.object(), Some(obj(ObjectKind::Table, "order_items")));
        let col = &s.lookup(&NameQuery::new("total"))[0];
        assert_eq!(col.kind, NameKind::Column);
        assert_eq!(col.name, "Total");
        assert_eq!(col.parent, main().in_table("orders"));
        assert_eq!(col.object(), None);
    }

    #[test]
    fn relations_are_found_ignoring_case() {
        let s = snapshot();
        let found = s.find_relations(&ObjectPath::root(), "USERS");
        assert_eq!(found, vec![obj(ObjectKind::Table, "Users")]);
        assert_eq!(s.find_relations(&main(), "nope").len(), 0);
        let view = s.find_relations(&main(), "USER_VIEW");
        assert_eq!(view, vec![obj(ObjectKind::View, "user_view")]);
    }

    #[test]
    fn table_state_distinguishes_unloaded_empty_and_failed() {
        let mut store = Store::default();
        let (o, e) = table("empty", &[]);
        let bad = obj(ObjectKind::Table, "bad");
        store.apply(Staged {
            tables: vec![(o.clone(), e)],
            failures: vec![(
                CacheKey::Table(bad.clone()),
                CatalogError::Driver("boom".into()),
            )],
            ..Staged::default()
        });
        let s = CatalogSnapshot::new(Arc::new(store));
        match s.table_state(&o) {
            TableState::Loaded(t) => assert_eq!(t.columns.len(), 0),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            s.table_state(&bad),
            TableState::Failed(CatalogError::Driver("boom".into()))
        );
        assert_eq!(
            s.table_state(&obj(ObjectKind::Table, "x")),
            TableState::NotLoaded
        );
    }

    #[test]
    fn foreign_keys_resolve_the_target_schema() {
        let o = obj(ObjectKind::Table, "orders");
        let fks = vec![
            ForeignKey::new(vec!["c".into()], "customers", vec!["id".into()]).with_name("fk1"),
            ForeignKey::new(vec!["a".into()], "accounts", vec!["id".into()])
                .with_referenced_schema("billing"),
        ];
        let t = Table::new(o.clone()).with_foreign_keys(fks);
        let mut store = Store::default();
        store.apply(Staged {
            tables: vec![(o.clone(), Arc::new(TableEntry::new(Arc::new(t))))],
            ..Staged::default()
        });
        let s = CatalogSnapshot::new(Arc::new(store));
        let got = s.foreign_keys(&o).unwrap();
        assert_eq!(got[0].target, obj(ObjectKind::Table, "customers"));
        assert_eq!(got[0].name.as_deref(), Some("fk1"));
        assert_eq!(
            got[1].target,
            DbObject::new(
                ObjectKind::Table,
                ObjectPath::root().in_schema("billing"),
                "accounts"
            )
        );
        assert_eq!(got[1].columns, ["a"]);
        assert!(s.foreign_keys(&obj(ObjectKind::Table, "zz")).is_none());
    }

    #[test]
    fn index_bytes_and_generation_are_reported() {
        let s = snapshot();
        assert!(s.index_bytes() > 0);
        assert_eq!(s.generation(), 1);
    }
}
