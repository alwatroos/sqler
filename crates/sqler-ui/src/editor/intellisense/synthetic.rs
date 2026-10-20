// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! A synthetic database for the latency bench and the tests: thousands of tables behind the catalog.
//!
//! The source has the schemas `public` and `s_00` to `s_19` (ten tables each, `u_0` to `u_9`), and the
//! tables `t_00000` onwards in `public`. Every table has `id` (integer, not null) and the text columns
//! `c_1` onwards; `c_1` references `t_{n-1}(id)`. Views and materialised views are empty, every other
//! kind is unsupported.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sqler_catalog::{Catalog, CatalogWorker};
use sqler_driver_api::sqler_core::{
    ColumnInfo, DataType, DbObject, ForeignKey, Listing, ObjectKind, ObjectPath, Table,
};
use sqler_driver_api::{BoxFuture, DriverError, Introspect};

/// Number of extra schemas `s_00` onwards.
const EXTRA_SCHEMAS: usize = 20;
/// Tables in each extra schema.
const EXTRA_TABLES: usize = 10;

/// Changes the synthetic source while the catalog runs, to test refreshes.
#[derive(Debug, Clone)]
pub struct SyntheticHandle {
    tables: Arc<AtomicUsize>,
}

impl SyntheticHandle {
    /// Sets the number of tables in `public`; a refresh picks it up.
    pub fn set_tables(&self, tables: usize) {
        self.tables.store(tables, Ordering::Relaxed);
    }
}

struct SyntheticIntrospect {
    tables: Arc<AtomicUsize>,
    columns: usize,
}

fn public() -> ObjectPath {
    ObjectPath::root().in_schema("public")
}

impl Introspect for SyntheticIntrospect {
    fn supports(&self, kind: ObjectKind) -> bool {
        matches!(
            kind,
            ObjectKind::Schema
                | ObjectKind::Table
                | ObjectKind::View
                | ObjectKind::MaterializedView
        )
    }

    fn list<'a>(
        &'a mut self,
        parent: &'a ObjectPath,
        kind: ObjectKind,
    ) -> BoxFuture<'a, Result<Listing, DriverError>> {
        let listing = match kind {
            ObjectKind::Schema => {
                let root = ObjectPath::root();
                let mut schemas = vec![DbObject::new(kind, root.clone(), "public")];
                schemas.extend(
                    (0..EXTRA_SCHEMAS)
                        .map(|i| DbObject::new(kind, root.clone(), format!("s_{i:02}"))),
                );
                Listing::Objects(schemas)
            }
            ObjectKind::Table => match parent.schema.as_deref() {
                None | Some("public") => {
                    let path = public();
                    Listing::Objects(
                        (0..self.tables.load(Ordering::Relaxed))
                            .map(|i| DbObject::new(kind, path.clone(), format!("t_{i:05}")))
                            .collect(),
                    )
                }
                Some(_) => Listing::Objects(
                    (0..EXTRA_TABLES)
                        .map(|i| DbObject::new(kind, parent.clone(), format!("u_{i}")))
                        .collect(),
                ),
            },
            ObjectKind::View | ObjectKind::MaterializedView => Listing::Objects(Vec::new()),
            _ => Listing::Unsupported,
        };
        Box::pin(async move { Ok(listing) })
    }

    fn table<'a>(
        &'a mut self,
        table: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<Table>, DriverError>> {
        let result = (table.kind == ObjectKind::Table).then(|| {
            let mut columns = vec![ColumnInfo::new("id", DataType::Integer)];
            columns[0].nullable = false;
            columns.extend(
                (1..self.columns).map(|i| ColumnInfo::new(format!("c_{i}"), DataType::Text)),
            );
            let mut out = Table::new(table.clone()).with_columns(columns);
            let previous = table
                .name
                .strip_prefix("t_")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1));
            if let (Some(previous), true) = (previous, self.columns > 1) {
                out = out.with_foreign_keys(vec![ForeignKey::new(
                    vec!["c_1".to_owned()],
                    format!("t_{previous:05}"),
                    vec!["id".to_owned()],
                )]);
            }
            out
        });
        Box::pin(async move { Ok(result) })
    }

    fn definition<'a>(
        &'a mut self,
        _object: &'a DbObject,
    ) -> BoxFuture<'a, Result<Option<String>, DriverError>> {
        Box::pin(async { Ok(None) })
    }
}

/// A catalog over the synthetic source with `tables` tables in `public` and `columns` columns per table
/// (`id` plus `c_1` onwards). Run the worker on a tokio runtime.
pub fn synthetic_catalog(tables: usize, columns: usize) -> (Catalog, CatalogWorker) {
    let (catalog, worker, _) = synthetic_catalog_with(tables, columns);
    (catalog, worker)
}

/// Like [`synthetic_catalog`], with a handle that changes the source later.
pub fn synthetic_catalog_with(
    tables: usize,
    columns: usize,
) -> (Catalog, CatalogWorker, SyntheticHandle) {
    let tables = Arc::new(AtomicUsize::new(tables));
    let source: Box<dyn Introspect> = Box::new(SyntheticIntrospect {
        tables: Arc::clone(&tables),
        columns,
    });
    let (catalog, worker) = Catalog::new(source);
    (catalog, worker, SyntheticHandle { tables })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_is_shaped_as_documented() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let mut source = SyntheticIntrospect {
            tables: Arc::new(AtomicUsize::new(3)),
            columns: 3,
        };
        rt.block_on(async {
            let root = ObjectPath::root();
            let Ok(Listing::Objects(schemas)) = source.list(&root, ObjectKind::Schema).await else {
                panic!("schemas");
            };
            assert_eq!(schemas.len(), 21);
            let Ok(Listing::Objects(tables)) = source.list(&root, ObjectKind::Table).await else {
                panic!("tables");
            };
            assert_eq!(tables.len(), 3);
            assert_eq!(tables[0].parent.schema.as_deref(), Some("public"));
            let extra = ObjectPath::root().in_schema("s_04");
            let Ok(Listing::Objects(own)) = source.list(&extra, ObjectKind::Table).await else {
                panic!("own tables");
            };
            assert_eq!(own.len(), 10);
            assert_eq!(own[0].name, "u_0");
            assert!(matches!(
                source.list(&root, ObjectKind::Function).await,
                Ok(Listing::Unsupported)
            ));
            let Ok(Some(t)) = source.table(&tables[2]).await else {
                panic!("table");
            };
            let names: Vec<_> = t.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["id", "c_1", "c_2"]);
            assert!(!t.columns[0].nullable && t.columns[1].nullable);
            assert_eq!(t.foreign_keys[0].referenced_table, "t_00001");
            let Ok(Some(first)) = source.table(&tables[0]).await else {
                panic!("first");
            };
            assert_eq!(first.foreign_keys.len(), 0);
        });
    }
}
