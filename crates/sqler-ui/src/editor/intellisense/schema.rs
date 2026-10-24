// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! [`CatalogSchema`]: the engine's [`SchemaSource`] over one catalog snapshot.

use std::cell::OnceCell;
use std::sync::Arc;

use sqler_catalog::{CatalogSnapshot, NodeKey, TableState};
use sqler_completion::{Loadable, Relation, SchemaSource};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, Table,
};
use sqler_sql::Dialect;

/// The kinds that make up the relations of a schema, in listing order.
pub(super) const RELATION_KINDS: [ObjectKind; 3] = [
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::MaterializedView,
];

/// What is known about the details of one listed relation.
enum Slot {
    Table(Arc<Table>),
    NotLoaded,
    /// Failed to load, or the details are not available: reads as "no columns".
    Empty,
}

/// One listing of relations as the snapshot holds it, with a lazily filled slot per object.
struct Listed {
    listing: Option<Arc<Listing>>,
    slots: OnceCell<Vec<OnceCell<Slot>>>,
}

impl Listed {
    fn new(snapshot: &CatalogSnapshot, key: &NodeKey) -> Self {
        Self {
            listing: snapshot.listing(key),
            slots: OnceCell::new(),
        }
    }

    fn cached(&self) -> bool {
        self.listing.is_some()
    }

    fn objects(&self) -> &[DbObject] {
        match self.listing.as_deref() {
            Some(Listing::Objects(objects)) => objects,
            _ => &[],
        }
    }
}

/// How a schema name resolves against the cached listings.
enum Place<'a> {
    /// The schema's own listings, with the table listing cached.
    Own(&'a [Listed; 3]),
    /// The root listings stand in for it (it is the search-path schema).
    Root,
    /// The schema listing is cached and has no such schema.
    Missing,
    /// More needs loading to tell.
    Unloaded,
}

/// A [`SchemaSource`] over a [`CatalogSnapshot`].
///
/// Built per request from a fresh snapshot (ADR-0022), so it is cheap to build: it holds `Arc`s of the
/// cached listings and fills the per-object table state only when the engine asks. Names are exact
/// stored names, as the trait requires.
pub struct CatalogSchema {
    snapshot: CatalogSnapshot,
    search_path: Vec<String>,
    root: ObjectPath,
    schemas: Option<Arc<Listing>>,
    /// Parallel to the objects of `schemas`.
    schema_lists: Vec<OnceCell<[Listed; 3]>>,
    root_lists: [Listed; 3],
}

impl std::fmt::Debug for CatalogSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogSchema")
            .field("generation", &self.snapshot.generation())
            .field("search_path", &self.search_path)
            .finish_non_exhaustive()
    }
}

impl CatalogSchema {
    /// Builds the source over `snapshot` for a connection of `dialect`.
    #[must_use]
    pub fn new(snapshot: CatalogSnapshot, dialect: Dialect) -> Self {
        let root = ObjectPath::root();
        let schemas = snapshot.listing(&NodeKey::new(root.clone(), ObjectKind::Schema));
        let count = match schemas.as_deref() {
            Some(Listing::Objects(objects)) => objects.len(),
            _ => 0,
        };
        let root_lists =
            RELATION_KINDS.map(|kind| Listed::new(&snapshot, &NodeKey::new(root.clone(), kind)));
        let from_listing = root_lists
            .iter()
            .find_map(|list| list.objects().first())
            .and_then(|obj| obj.parent.schema.clone());
        let search_path = from_listing
            .or_else(|| match dialect {
                Dialect::Postgres => Some("public".to_owned()),
                Dialect::Sqlite => Some("main".to_owned()),
                Dialect::TSql => Some("dbo".to_owned()),
                _ => None,
            })
            .into_iter()
            .collect();
        Self {
            snapshot,
            search_path,
            root,
            schemas,
            schema_lists: (0..count).map(|_| OnceCell::new()).collect(),
            root_lists,
        }
    }

    fn schema_objects(&self) -> &[DbObject] {
        match self.schemas.as_deref() {
            Some(Listing::Objects(objects)) => objects,
            _ => &[],
        }
    }

    fn schema_index(&self, name: &str) -> Option<usize> {
        self.schema_objects()
            .iter()
            .position(|o| o.kind == ObjectKind::Schema && o.name == name)
    }

    fn is_search_path(&self, schema: &str) -> bool {
        self.search_path.first().is_some_and(|s| s == schema)
    }

    fn own_lists(&self, index: usize) -> Option<&[Listed; 3]> {
        let obj = self.schema_objects().get(index)?;
        let cell = self.schema_lists.get(index)?;
        Some(cell.get_or_init(|| {
            let path = obj.child_path();
            RELATION_KINDS
                .map(|kind| Listed::new(&self.snapshot, &NodeKey::new(path.clone(), kind)))
        }))
    }

    fn place(&self, schema: &str) -> Place<'_> {
        let own = self.schema_index(schema).and_then(|i| self.own_lists(i));
        match own {
            Some(lists) if lists[0].cached() => Place::Own(lists),
            Some(_) if !self.is_search_path(schema) => Place::Unloaded,
            None if self.schemas.is_some() && !self.is_search_path(schema) => Place::Missing,
            None if self.schemas.is_none() && !self.is_search_path(schema) => Place::Unloaded,
            _ => Place::Root,
        }
    }

    /// The listings a (schema, relation) lookup searches.
    fn lookup_lists(&self, schema: Option<&str>) -> Vec<&Listed> {
        match schema {
            None => {
                let mut lists: Vec<&Listed> = self.root_lists.iter().collect();
                if let Some(Place::Own(own)) = self.search_path.first().map(|s| self.place(s)) {
                    lists.extend(own.iter());
                }
                lists
            }
            Some(name) => match self.place(name) {
                Place::Own(own) => own.iter().collect(),
                Place::Root => self.root_lists.iter().collect(),
                Place::Missing | Place::Unloaded => Vec::new(),
            },
        }
    }

    fn find<'a>(
        &'a self,
        schema: Option<&str>,
        relation: &str,
        matches: &dyn Fn(&str, &str) -> bool,
    ) -> Option<(&'a Listed, usize)> {
        self.lookup_lists(schema).into_iter().find_map(|list| {
            list.objects()
                .iter()
                .position(|o| matches(&o.name, relation))
                .map(|i| (list, i))
        })
    }

    fn slot<'a>(&self, list: &'a Listed, index: usize) -> &'a Slot {
        static EMPTY: Slot = Slot::Empty;
        let slots = list
            .slots
            .get_or_init(|| (0..list.objects().len()).map(|_| OnceCell::new()).collect());
        let (Some(cell), Some(obj)) = (slots.get(index), list.objects().get(index)) else {
            return &EMPTY;
        };
        cell.get_or_init(|| match self.snapshot.table_state(obj) {
            TableState::Loaded(table) => Slot::Table(table),
            TableState::NotLoaded => Slot::NotLoaded,
            _ => Slot::Empty,
        })
    }

    /// The relation named exactly `relation` in `schema` (`None`: the search path), if listed.
    #[must_use]
    pub fn object(&self, schema: Option<&str>, relation: &str) -> Option<DbObject> {
        self.find(schema, relation, &|a, b| a == b)
            .and_then(|(list, i)| list.objects().get(i).cloned())
    }

    /// Like [`CatalogSchema::object`], but an exact name wins and otherwise a case-insensitive one is taken.
    ///
    /// `schema` is also matched exactly first, then case-insensitively.
    #[must_use]
    pub(super) fn object_folded(&self, schema: Option<&str>, relation: &str) -> Option<DbObject> {
        let schema = schema.map(|s| self.schema_folded(s));
        self.object(schema, relation).or_else(|| {
            let fold = |s: &str| s.to_lowercase();
            let wanted = fold(relation);
            self.find(schema, relation, &|name, _| fold(name) == wanted)
                .and_then(|(list, i)| list.objects().get(i).cloned())
        })
    }

    fn schema_folded<'a>(&'a self, name: &'a str) -> &'a str {
        if self.schema_index(name).is_some() {
            return name;
        }
        let wanted = name.to_lowercase();
        self.schema_objects()
            .iter()
            .find(|o| o.kind == ObjectKind::Schema && o.name.to_lowercase() == wanted)
            .map_or(name, |o| o.name.as_str())
    }

    /// Whether the table details of `object` are still to be loaded.
    #[must_use]
    pub(super) fn needs_table(&self, object: &DbObject) -> bool {
        matches!(self.snapshot.table_state(object), TableState::NotLoaded)
    }

    /// The path of the children of `schema`, if the root schema listing has it.
    #[must_use]
    pub fn schema_path(&self, schema: &str) -> Option<ObjectPath> {
        let obj = self.schema_objects().get(self.schema_index(schema)?)?;
        Some(obj.child_path())
    }

    /// The path of the server root.
    pub(super) fn root(&self) -> &ObjectPath {
        &self.root
    }
}

impl SchemaSource for CatalogSchema {
    fn schemas(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.schema_objects()
                .iter()
                .filter(|o| o.kind == ObjectKind::Schema)
                .map(|o| o.name.as_str()),
        )
    }

    fn search_path(&self) -> &[String] {
        &self.search_path
    }

    fn relations(
        &self,
        schema: Option<&str>,
    ) -> Loadable<Box<dyn Iterator<Item = Relation<'_>> + '_>> {
        let lists: &[Listed; 3] = match schema.map(|s| self.place(s)) {
            None | Some(Place::Root) => {
                if !self.root_lists[0].cached() {
                    return Loadable::NotLoaded;
                }
                &self.root_lists
            }
            Some(Place::Own(own)) => own,
            Some(Place::Missing) => return Loadable::Loaded(Box::new(std::iter::empty())),
            Some(Place::Unloaded) => return Loadable::NotLoaded,
        };
        Loadable::Loaded(Box::new(lists.iter().flat_map(|list| {
            list.objects()
                .iter()
                .map(|o| Relation::new(o.parent.schema.as_deref(), &o.name, o.kind))
        })))
    }

    fn columns(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ColumnInfo]> {
        match self.find(schema, relation, &|a, b| a == b) {
            Some((list, i)) => match self.slot(list, i) {
                Slot::Table(table) => Loadable::Loaded(&table.columns),
                Slot::NotLoaded => Loadable::NotLoaded,
                Slot::Empty => Loadable::Loaded(&[]),
            },
            None => Loadable::Loaded(&[]),
        }
    }

    fn foreign_keys(&self, schema: Option<&str>, relation: &str) -> Loadable<&[ForeignKey]> {
        match self.find(schema, relation, &|a, b| a == b) {
            Some((list, i)) => match self.slot(list, i) {
                Slot::Table(table) => Loadable::Loaded(&table.foreign_keys),
                Slot::NotLoaded => Loadable::NotLoaded,
                Slot::Empty => Loadable::Loaded(&[]),
            },
            None => Loadable::Loaded(&[]),
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_catalog::Catalog;
    use sqler_driver_api::{BoxFuture, DriverError, Introspect};

    use super::super::keys::{bootstrap_keys, load};
    use super::super::synthetic::synthetic_catalog;
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("runtime")
    }

    fn setup(rt: &tokio::runtime::Runtime, tables: usize) -> Catalog {
        let (catalog, worker) = synthetic_catalog(tables, 4);
        rt.spawn(worker.run());
        catalog
    }

    fn names(schema: &CatalogSchema, from: Option<&str>) -> Vec<String> {
        match schema.relations(from) {
            Loadable::Loaded(it) => it.map(|r| r.name.to_owned()).collect(),
            Loadable::NotLoaded => vec!["<not loaded>".to_owned()],
        }
    }

    #[test]
    fn nothing_is_known_before_loading() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        assert_eq!(names(&s, None), ["<not loaded>"]);
        assert_eq!(names(&s, Some("public")), ["<not loaded>"]);
        assert_eq!(names(&s, Some("other")), ["<not loaded>"]);
        assert_eq!(s.schemas().count(), 0);
        assert_eq!(s.search_path(), ["public"]);
        let lite = CatalogSchema::new(catalog.snapshot(), Dialect::Sqlite);
        assert_eq!(lite.search_path(), ["main"]);
        let tsql = CatalogSchema::new(catalog.snapshot(), Dialect::TSql);
        assert_eq!(tsql.search_path(), ["dbo"]);
        let my = CatalogSchema::new(catalog.snapshot(), Dialect::MySql);
        assert_eq!(my.search_path().len(), 0);
    }

    #[test]
    fn bootstrap_gives_schemas_relations_and_search_path() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        rt.block_on(load(&catalog, bootstrap_keys()));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::MySql);
        assert_eq!(s.search_path(), ["public"]);
        assert_eq!(s.schemas().count(), 21);
        assert_eq!(names(&s, None).len(), 5);
        // `public` has no listing of its own yet: the root listings stand in for it.
        assert_eq!(names(&s, Some("public")).len(), 5);
        let first = match s.relations(None) {
            Loadable::Loaded(mut it) => it.next().map(|r| (r.schema.map(str::to_owned), r.kind)),
            Loadable::NotLoaded => None,
        };
        assert_eq!(first, Some((Some("public".to_owned()), ObjectKind::Table)));
        // Other schemas are not loaded; unknown ones are empty.
        assert_eq!(names(&s, Some("s_00")), ["<not loaded>"]);
        assert_eq!(names(&s, Some("nope")).len(), 0);
    }

    #[test]
    fn schema_listings_are_used_once_loaded() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        rt.block_on(load(&catalog, bootstrap_keys()));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        let path = s.schema_path("s_03").expect("schema path");
        assert_eq!(path.schema.as_deref(), Some("s_03"));
        assert!(s.schema_path("nope").is_none());
        let keys = super::super::keys::keys_for(
            &[sqler_completion::SchemaRequest::Relations {
                schema: Some("s_03".to_owned()),
            }],
            &s,
        );
        rt.block_on(load(&catalog, keys));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        assert_eq!(names(&s, Some("s_03")).len(), 10);
    }

    #[test]
    fn columns_are_not_loaded_until_asked_for() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        rt.block_on(load(&catalog, bootstrap_keys()));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        assert!(matches!(
            s.columns(Some("public"), "t_00002"),
            Loadable::NotLoaded
        ));
        assert!(matches!(s.columns(None, "t_00002"), Loadable::NotLoaded));
        assert!(matches!(s.columns(None, "nope"), Loadable::Loaded(&[])));
        let obj = s.object(None, "t_00002").expect("listed");
        rt.block_on(load(&catalog, vec![sqler_catalog::CacheKey::Table(obj)]));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        let Loadable::Loaded(cols) = s.columns(Some("public"), "t_00002") else {
            panic!("columns should be loaded");
        };
        assert_eq!(cols.first().map(|c| c.name.as_str()), Some("id"));
        let Loadable::Loaded(fks) = s.foreign_keys(None, "t_00002") else {
            panic!("keys should be loaded");
        };
        assert_eq!(
            fks.first().map(|f| f.referenced_table.as_str()),
            Some("t_00001")
        );
        // Exact names only.
        assert!(matches!(s.columns(None, "T_00002"), Loadable::Loaded(&[])));
    }

    /// Lists one table, `bad`, that cannot be described.
    struct Failing;

    impl Introspect for Failing {
        fn supports(&self, kind: ObjectKind) -> bool {
            kind == ObjectKind::Table
        }

        fn list<'a>(
            &'a mut self,
            parent: &'a ObjectPath,
            kind: ObjectKind,
        ) -> BoxFuture<'a, Result<Listing, DriverError>> {
            let listing = if kind == ObjectKind::Table {
                Listing::Objects(vec![DbObject::new(
                    kind,
                    parent.clone().in_schema("main"),
                    "bad",
                )])
            } else {
                Listing::Unsupported
            };
            Box::pin(async move { Ok(listing) })
        }

        fn table<'a>(
            &'a mut self,
            _table: &'a DbObject,
        ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
            Box::pin(async { Err(DriverError::Query("boom".to_owned())) })
        }

        fn definition<'a>(
            &'a mut self,
            _object: &'a DbObject,
        ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
            Box::pin(async { Ok(None) })
        }
    }

    #[test]
    fn a_failed_table_reads_as_no_columns() {
        let rt = runtime();
        let boxed: Box<dyn Introspect> = Box::new(Failing);
        let (catalog, worker) = Catalog::new(boxed);
        rt.spawn(worker.run());
        rt.block_on(load(&catalog, bootstrap_keys()));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Sqlite);
        assert_eq!(s.search_path(), ["main"]);
        assert!(matches!(s.columns(None, "bad"), Loadable::NotLoaded));
        let obj = s.object(None, "bad").expect("listed");
        rt.block_on(load(&catalog, vec![sqler_catalog::CacheKey::Table(obj)]));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Sqlite);
        assert!(matches!(s.columns(None, "bad"), Loadable::Loaded(&[])));
        assert!(matches!(s.foreign_keys(None, "bad"), Loadable::Loaded(&[])));
    }

    #[test]
    fn object_folded_prefers_exact_then_ignores_case() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        rt.block_on(load(&catalog, bootstrap_keys()));
        let s = CatalogSchema::new(catalog.snapshot(), Dialect::Postgres);
        assert_eq!(
            s.object_folded(None, "t_00001").map(|o| o.name),
            Some("t_00001".to_owned())
        );
        assert_eq!(
            s.object_folded(None, "T_00001").map(|o| o.name),
            Some("t_00001".to_owned())
        );
        assert_eq!(
            s.object_folded(Some("PUBLIC"), "T_00001").map(|o| o.name),
            Some("t_00001".to_owned())
        );
        assert!(s.object_folded(None, "zzz").is_none());
        assert!(s.object(None, "T_00001").is_none());
    }
}
