// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! From what the engine reports as missing to catalog cache keys, and the loading of those keys.

use std::collections::HashSet;
use std::future::Future;

use sqler_catalog::{CacheKey, Catalog, NodeKey};
use sqler_completion::{RelationSource, SchemaRequest, analyze};
use sqler_driver_api::sqler_core::{DbObject, ObjectKind, ObjectPath};
use sqler_sql::{Dialect, split};

use super::schema::{CatalogSchema, RELATION_KINDS};

/// The listing of schemas and the relation listings of the current schema.
///
/// A `None` schema in a parent path means the session's current schema, so these are the first
/// loads of a connection.
#[must_use]
pub fn bootstrap_keys() -> Vec<CacheKey> {
    let root = ObjectPath::root();
    let mut keys = vec![CacheKey::Children(NodeKey::new(
        root.clone(),
        ObjectKind::Schema,
    ))];
    keys.extend(relation_keys(&root));
    keys
}

fn relation_keys(path: &ObjectPath) -> impl Iterator<Item = CacheKey> + '_ {
    RELATION_KINDS
        .into_iter()
        .map(|kind| CacheKey::Children(NodeKey::new(path.clone(), kind)))
}

fn keys_for_schema(schema: Option<&str>, source: &CatalogSchema) -> Vec<CacheKey> {
    match schema {
        None => bootstrap_keys(),
        Some(name) => match source.schema_path(name) {
            Some(path) => relation_keys(&path).collect(),
            None => vec![CacheKey::Children(NodeKey::new(
                source.root().clone(),
                ObjectKind::Schema,
            ))],
        },
    }
}

/// The cache keys that answer `requests`, without duplicates, in first-seen order.
#[must_use]
pub fn keys_for(requests: &[SchemaRequest], schema: &CatalogSchema) -> Vec<CacheKey> {
    let mut keys = Vec::new();
    for request in requests {
        match request {
            SchemaRequest::Relations { schema: name } => {
                keys.extend(keys_for_schema(name.as_deref(), schema));
            }
            SchemaRequest::Columns {
                schema: name,
                relation,
            } => match schema.object(name.as_deref(), relation) {
                Some(object) => keys.push(CacheKey::Table(object)),
                None => keys.extend(keys_for_schema(name.as_deref(), schema)),
            },
            _ => {}
        }
    }
    let mut seen = HashSet::new();
    keys.retain(|key| seen.insert(key.clone()));
    keys
}

/// The table loads for the relations that `text` names and `schema` has not loaded yet, at most `limit`.
///
/// A name matches exactly, else case-insensitively. Relations that are not listed are skipped.
#[must_use]
pub fn prefetch_keys(
    text: &str,
    dialect: Dialect,
    schema: &CatalogSchema,
    limit: usize,
) -> Vec<CacheKey> {
    let mut seen: HashSet<DbObject> = HashSet::new();
    let mut keys = Vec::new();
    for segment in split(dialect, text) {
        if segment.body.is_empty() {
            continue;
        }
        let mut analysis = analyze(text, segment.body.end, dialect);
        if analysis.scope.relations.is_empty() {
            // A statement ending in a number, string or comment has no scope at its end.
            analysis = analyze(text, segment.body.start, dialect);
        }
        for relation in &analysis.scope.relations {
            let RelationSource::Table { schema: qual, name } = &relation.source else {
                continue;
            };
            let Some(object) =
                schema.object_folded(qual.as_ref().map(|q| q.value.as_str()), &name.value)
            else {
                continue;
            };
            if !schema.needs_table(&object) || !seen.insert(object.clone()) {
                continue;
            }
            if keys.len() >= limit {
                return keys;
            }
            keys.push(CacheKey::Table(object));
        }
    }
    keys
}

/// Loads `keys` through `catalog`. Errors are ignored; they stay in [`Catalog::node_error`].
///
/// The table loads are sent when this is called; the future then waits for them and loads the
/// listings one after the other. It does not need a runtime of its own: the catalog worker does the work.
pub fn load(catalog: &Catalog, keys: Vec<CacheKey>) -> impl Future<Output = ()> + Send + 'static {
    let mut tables = Vec::new();
    let mut lists = Vec::new();
    for key in keys {
        match key {
            CacheKey::Table(object) => tables.push(object),
            CacheKey::Children(node) => lists.push(node),
            _ => {}
        }
    }
    let loading = (!tables.is_empty()).then(|| catalog.load_tables(tables));
    let catalog = catalog.clone();
    async move {
        if let Some(loading) = loading {
            let _ = loading.await;
        }
        for node in lists {
            let _ = catalog.children(node).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use sqler_catalog::RefreshScope;
    use sqler_completion::{CompletionRequest, SchemaSource, complete};

    use super::super::popup::complete_popup;
    use super::super::synthetic::{SyntheticHandle, synthetic_catalog_with};
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("runtime")
    }

    fn setup(rt: &tokio::runtime::Runtime, tables: usize) -> Catalog {
        setup_with(rt, tables).0
    }

    fn setup_with(rt: &tokio::runtime::Runtime, tables: usize) -> (Catalog, SyntheticHandle) {
        let (catalog, worker, handle) = synthetic_catalog_with(tables, 4);
        rt.spawn(worker.run());
        rt.block_on(load(&catalog, bootstrap_keys()));
        (catalog, handle)
    }

    fn fresh(catalog: &Catalog) -> CatalogSchema {
        CatalogSchema::new(catalog.snapshot(), Dialect::Postgres)
    }

    fn list_key(path: ObjectPath, kind: ObjectKind) -> CacheKey {
        CacheKey::Children(NodeKey::new(path, kind))
    }

    #[test]
    fn every_key_marked_asked_is_requested_and_loaded() {
        // The view marks the bootstrap keys asked through `fresh` and hands exactly those to
        // `load`; none may stay unloaded, or `fresh` would hide it for the whole binding.
        let rt = runtime();
        let (catalog, worker, _) = synthetic_catalog_with(5, 4);
        rt.spawn(worker.run());
        let mut tab = super::super::tab::TabIntel::default();
        tab.bind(Some(1));
        let asked = tab.fresh(bootstrap_keys());
        assert_eq!(asked, bootstrap_keys());
        rt.block_on(load(&catalog, asked.clone()));
        for key in asked {
            let CacheKey::Children(node) = key else {
                panic!("a listing")
            };
            assert!(catalog.cached(&node).is_some(), "{node:?} was not loaded");
        }
        assert!(tab.fresh(bootstrap_keys()).is_empty());
    }

    #[test]
    fn bootstrap_is_schemas_and_current_relations() {
        let root = ObjectPath::root();
        assert_eq!(
            bootstrap_keys(),
            vec![
                list_key(root.clone(), ObjectKind::Schema),
                list_key(root.clone(), ObjectKind::Table),
                list_key(root.clone(), ObjectKind::View),
                list_key(root, ObjectKind::MaterializedView),
            ]
        );
    }

    #[test]
    fn keys_per_request_kind_deduplicated() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        let s = fresh(&catalog);
        let none = SchemaRequest::Relations { schema: None };
        assert_eq!(keys_for(std::slice::from_ref(&none), &s), bootstrap_keys());

        let some = SchemaRequest::Relations {
            schema: Some("s_01".to_owned()),
        };
        let path = ObjectPath::root().in_schema("s_01");
        assert_eq!(
            keys_for(std::slice::from_ref(&some), &s),
            vec![
                list_key(path.clone(), ObjectKind::Table),
                list_key(path.clone(), ObjectKind::View),
                list_key(path, ObjectKind::MaterializedView),
            ]
        );
        let unknown = SchemaRequest::Relations {
            schema: Some("nope".to_owned()),
        };
        assert_eq!(
            keys_for(&[unknown], &s),
            vec![list_key(ObjectPath::root(), ObjectKind::Schema)]
        );

        let columns = SchemaRequest::Columns {
            schema: None,
            relation: "t_00002".to_owned(),
        };
        let obj = s.object(None, "t_00002").expect("listed");
        assert_eq!(
            keys_for(std::slice::from_ref(&columns), &s),
            vec![CacheKey::Table(obj.clone())]
        );
        let missing = SchemaRequest::Columns {
            schema: Some("s_02".to_owned()),
            relation: "x".to_owned(),
        };
        assert_eq!(keys_for(&[missing], &s).len(), 3);

        let all = keys_for(&[none.clone(), columns.clone(), none, columns], &s);
        assert_eq!(all.len(), 5);
        assert_eq!(all.last(), Some(&CacheKey::Table(obj)));
    }

    #[test]
    fn prefetch_matches_case_insensitively_and_limits() {
        let rt = runtime();
        let catalog = setup(&rt, 20);
        let s = fresh(&catalog);
        let text = "SELECT * FROM T_00001 a JOIN t_00002 b ON 1=1 JOIN nope n ON 1=1;\nSELECT * FROM t_00003";
        let keys = prefetch_keys(text, Dialect::Postgres, &s, 64);
        let names: Vec<_> = keys
            .iter()
            .filter_map(|k| match k {
                CacheKey::Table(o) => Some(o.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["t_00001", "t_00002", "t_00003"]);
        assert_eq!(prefetch_keys(text, Dialect::Postgres, &s, 2).len(), 2);
        assert_eq!(prefetch_keys(text, Dialect::Postgres, &s, 0).len(), 0);

        rt.block_on(load(&catalog, keys));
        let loaded = fresh(&catalog);
        assert_eq!(prefetch_keys(text, Dialect::Postgres, &loaded, 64).len(), 0);
    }

    #[test]
    fn load_ignores_errors_and_fills_the_cache() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        let s = fresh(&catalog);
        let obj = s.object(None, "t_00001").expect("listed");
        let ghost = DbObject::new(
            ObjectKind::Table,
            ObjectPath::root().in_schema("public"),
            "ghost",
        );
        rt.block_on(load(
            &catalog,
            vec![
                CacheKey::Table(obj.clone()),
                CacheKey::Table(ghost),
                list_key(ObjectPath::root().in_schema("s_00"), ObjectKind::Table),
            ],
        ));
        assert!(catalog.cached_table(&obj).is_some());
        assert!(
            catalog
                .cached(&NodeKey::new(
                    ObjectPath::root().in_schema("s_00"),
                    ObjectKind::Table
                ))
                .is_some()
        );
    }

    #[test]
    fn popup_flow_loads_columns_on_demand() {
        let rt = runtime();
        let catalog = setup(&rt, 5);
        let text = "SELECT u. FROM t_00001 u";
        let offset = "SELECT u.".len();
        let first = complete_popup(text, offset, Dialect::Postgres, &fresh(&catalog));
        assert!(
            first
                .items
                .iter()
                .all(|i| i.label != "id" && i.label != "c_1")
        );
        let s = fresh(&catalog);
        let expected = s.object(None, "t_00001").expect("listed");
        assert_eq!(first.loads, vec![CacheKey::Table(expected)]);

        rt.block_on(load(&catalog, first.loads));
        let second = complete_popup(text, offset, Dialect::Postgres, &fresh(&catalog));
        let labels: Vec<_> = second.items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"id"), "{labels:?}");
        assert!(labels.contains(&"c_1"), "{labels:?}");
        assert_eq!(second.loads.len(), 0);
        // The raw engine agrees.
        let done = complete(
            &CompletionRequest::new(text, offset, Dialect::Postgres),
            &fresh(&catalog),
        );
        assert_eq!(done.requests.len(), 0);
    }

    #[test]
    fn a_refresh_shows_up_in_the_next_snapshot() {
        let rt = runtime();
        let (catalog, handle) = setup_with(&rt, 5);
        let generation = catalog.snapshot().generation();
        let before = fresh(&catalog);
        let text = "SELECT * FROM ";
        let has = |s: &CatalogSchema, name: &str| {
            complete_popup(text, text.len(), Dialect::Postgres, s)
                .items
                .iter()
                .any(|i| i.label == name)
        };
        assert!(has(&before, "t_00004"));
        assert!(!has(&before, "t_00005"));
        handle.set_tables(6);
        let mut handle = catalog.refresh(RefreshScope::All);
        rt.block_on(async { while handle.next_event().await.is_some() {} });
        let after = fresh(&catalog);
        assert!(has(&after, "t_00005"));
        assert_eq!(after.search_path(), ["public"]);
        assert_ne!(generation, catalog.snapshot().generation());
    }
}
